//! Stamps the build with its commit (`DOMOVOI_BUILD_COMMIT`), and gives the
//! Windows executable its icon (apps/server/icon/domovoi-server.ico, made by
//! tools/icons.py). The icon needs the resource compiler (rc.exe with MSVC,
//! windres with MinGW); without one the build goes on without it.

fn main() {
    println!("cargo:rerun-if-changed=icon/domovoi-server.ico");
    println!("cargo:rustc-env=DOMOVOI_BUILD_COMMIT={}", commit());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The icon is kept in Git LFS: a checkout without `git lfs pull` holds a
    // pointer file, not an icon.
    if !std::fs::read("icon/domovoi-server.ico").is_ok_and(|bytes| bytes.starts_with(&[0, 0, 1, 0])) {
        println!("cargo:warning=icon/domovoi-server.ico is not an icon (run `git lfs pull`); domovoi-server.exe gets no icon");
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("icon/domovoi-server.ico").set("FileDescription", "Domovoi Server").set("ProductName", "Domovoi");
    if let Err(e) = resource.compile() {
        println!("cargo:warning=domovoi-server.exe gets no icon: the resource compiler failed ({e})");
    }
}

/// The commit the build is of, `-dirty` with tracked changes on top;
/// `unknown` outside a git checkout.
#[expect(clippy::disallowed_methods, reason = "a build script has no `crate::process`; git runs once, to its end")]
fn commit() -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    // A new commit or a change to a tracked file builds again.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/index");
        if let Some(head) = std::fs::read_to_string(format!("{dir}/HEAD")).ok().and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_owned())) {
            println!("cargo:rerun-if-changed={dir}/{head}");
        }
    }
    let Some(sha) = git(&["rev-parse", "--short=10", "HEAD"]).filter(|s| !s.is_empty()) else { return "unknown".to_owned() };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    if dirty { format!("{sha}-dirty") } else { sha }
}
