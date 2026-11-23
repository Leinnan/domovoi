//! Git LFS files in a workspace: pointer files (spec v1), the repository's
//! local object cache, and fetching one object with git-lfs.
//!
//! Reading never runs git-lfs, its filters, remotes or credentials: an object
//! already in the cache is read from `<git common dir>/<lfs.storage>/objects`.
//! Only `fetch`, which a user asks for, reaches the network.

use crate::{process, util::sha256_hex};
use domovoi_model::typed::LfsObjectState;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::{self, File};
use tokio::io::AsyncReadExt;

/// A pointer file is shorter than this.
pub const POINTER_LIMIT: usize = 1024;
const VERSION: &str = "version https://git-lfs.github.com/spec/v1";
/// A local git command: a path, a config value, an attribute.
const LOCAL_GIT: Duration = Duration::from_secs(5);

/// What a pointer file names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// SHA-256 of the object, 64 lowercase hex digits.
    pub oid: String,
    pub size: u64,
    /// The pointer lists Git LFS extensions (`ext-0-name sha256:…`): the
    /// cache holds the object as they transformed it.
    pub extensions: bool,
}

impl Pointer {
    /// The pointer file's text, as git-lfs writes it (without extensions).
    pub fn text(&self) -> String {
        format!("{VERSION}\noid sha256:{}\nsize {}\n", self.oid, self.size)
    }
}

/// The pointer `bytes` are, if they are one: the version line, then
/// `ext-N-name sha256:<hex>` lines in order, `oid sha256:<hex>` and
/// `size <digits>`, and nothing after.
pub fn parse_pointer(bytes: &[u8]) -> Option<Pointer> {
    if bytes.len() >= POINTER_LIMIT || !bytes.starts_with(VERSION.as_bytes()) {
        return None;
    }
    let mut lines = std::str::from_utf8(bytes).ok()?.lines();
    if lines.next()? != VERSION {
        return None;
    }
    let mut line = lines.next()?;
    let mut extensions = false;
    let mut previous: Option<&str> = None;
    while let Some(ext) = line.strip_prefix("ext-") {
        let (key, value) = ext.split_once(' ')?;
        let (priority, name) = key.split_once('-')?;
        let valid = priority.len() == 1
            && priority.bytes().all(|b| b.is_ascii_digit())
            && !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && value.strip_prefix("sha256:").is_some_and(is_oid)
            && previous.is_none_or(|p| p < key);
        if !valid {
            return None;
        }
        previous = Some(key);
        extensions = true;
        line = lines.next()?;
    }
    let oid = line.strip_prefix("oid sha256:").filter(|oid| is_oid(oid))?;
    let size = lines.next()?.strip_prefix("size ")?;
    if lines.next().is_some() || size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Pointer { oid: oid.to_owned(), size: size.parse().ok()?, extensions })
}

fn is_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The first bytes of the file at `path`: enough to tell a pointer file.
pub async fn head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path).await?.take(POINTER_LIMIT as u64).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

/// `git` in `directory`, run to its end within `LOCAL_GIT`.
async fn git(directory: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut command = process::git(directory);
    command.args(args);
    process::output(command, LOCAL_GIT).await.ok()
}

/// The object store of the repository `directory` is in. Linked worktrees
/// share the main checkout's (`--git-common-dir`); `lfs.storage` moves it.
async fn store(directory: &Path) -> Option<PathBuf> {
    let common = git(directory, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await?;
    if !common.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8(common.stdout).ok()?.trim_end_matches(['\r', '\n']));
    if !common.is_absolute() {
        return None;
    }
    let storage = git(directory, &["config", "--null", "--get", "lfs.storage"]).await?;
    let storage = match storage.status.code()? {
        0 => common.join(std::str::from_utf8(storage.stdout.strip_suffix(b"\0")?).ok()?),
        1 => common.join("lfs"),
        _ => return None,
    };
    fs::canonicalize(storage.join("objects")).await.ok()
}

/// Where the cached object `oid` is, if it is there: a file inside the store.
async fn cached(directory: &Path, oid: &str) -> Option<PathBuf> {
    let store = store(directory).await?;
    let object = fs::canonicalize(store.join(&oid[..2]).join(&oid[2..4]).join(oid)).await.ok()?;
    (object.starts_with(&store) && fs::metadata(&object).await.ok()?.is_file()).then_some(object)
}

/// The object `pointer` names, read from the cache of the repository
/// `directory` is in and checked against its size and SHA-256, or why not:
/// `limit` bounds what is read.
pub async fn read_object(directory: &Path, pointer: &Pointer, limit: u64) -> Result<Vec<u8>, LfsObjectState> {
    if pointer.extensions {
        return Err(LfsObjectState::Unsupported);
    }
    let object = cached(directory, &pointer.oid).await.ok_or(LfsObjectState::Missing)?;
    if pointer.size > limit {
        return Err(LfsObjectState::TooLarge);
    }
    let mut contents = Vec::new();
    let file = File::open(object).await.map_err(|_| LfsObjectState::Missing)?;
    file.take(pointer.size + 1).read_to_end(&mut contents).await.map_err(|_| LfsObjectState::Mismatch)?;
    if contents.len() as u64 != pointer.size || sha256_hex(&contents) != pointer.oid {
        return Err(LfsObjectState::Mismatch);
    }
    Ok(contents)
}

/// Whether a `.gitattributes` rule puts `name` (a file in `directory`) in
/// Git LFS. `false` outside a repository.
pub async fn tracked(directory: &Path, name: &str) -> bool {
    let Some(out) = git(directory, &["check-attr", "-z", "filter", "--", name]).await else { return false };
    // `<path>\0filter\0<value>\0`
    out.status.success() && out.stdout.split(|&b| b == 0).nth(2) == Some(b"lfs".as_slice())
}

/// Downloads the object `pointer` names into the cache of the repository
/// `directory` is in, with git-lfs and the repository's remote and
/// credentials (`git lfs smudge`: the working tree is left as it is), then
/// checks the cached object. `name` is the file's, for git-lfs's messages.
pub async fn fetch(directory: &Path, name: &str, pointer: &Pointer) -> Result<LfsObjectState, String> {
    if pointer.extensions {
        return Ok(LfsObjectState::Unsupported);
    }
    if !git(directory, &["lfs", "version"]).await.is_some_and(|out| out.status.success()) {
        return Err("Git LFS isn't installed on the server. Install it (https://git-lfs.com), then try again.".into());
    }
    // An empty directory: the hooks path (no hook runs) and the pointer
    // given to git-lfs on stdin.
    let scratch = std::env::temp_dir().join(format!("domovoi-lfs-{}", crate::util::new_uuid()));
    let result = async {
        fs::create_dir(&scratch).await.map_err(|e| e.to_string())?;
        let pointer_file = scratch.join("pointer");
        fs::write(&pointer_file, pointer.text()).await.map_err(|e| e.to_string())?;
        let stdin = std::fs::File::open(&pointer_file).map_err(|e| e.to_string())?;
        let hooks = format!("core.hooksPath={}", scratch.join("hooks").display());
        let mut command = process::git(directory);
        command
            .args(["-c", &hooks, "lfs", "smudge", "--", name])
            .env("GIT_LFS_SKIP_SMUDGE", "0")
            .stdin(stdin)
            .stdout(std::process::Stdio::null());
        let out = process::output(command, process::SLOW).await.map_err(|e| format!("git lfs smudge: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git-lfs failed");
            return Err(format!("Git LFS couldn't fetch {name}: {reason}"));
        }
        Ok(())
    }
    .await;
    let _ = fs::remove_dir_all(&scratch).await;
    result?;
    Ok(match read_object(directory, pointer, u64::MAX).await {
        Ok(_) => LfsObjectState::Cached,
        Err(state) => state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    fn pointer(body: &str) -> Option<Pointer> {
        parse_pointer(format!("{VERSION}\n{body}").as_bytes())
    }

    #[test]
    fn a_pointer_is_its_version_oid_and_size() {
        assert_eq!(pointer(&format!("oid sha256:{OID}\nsize 12345\n")), Some(Pointer { oid: OID.into(), size: 12345, extensions: false }));
        // As `str::lines` reads them: no last newline, or CRLF.
        assert!(pointer(&format!("oid sha256:{OID}\nsize 1")).is_some());
        assert!(pointer(&format!("oid sha256:{OID}\r\nsize 1\r\n")).is_some());
        assert_eq!(parse_pointer(Pointer { oid: OID.into(), size: 7, extensions: false }.text().as_bytes()).map(|p| p.size), Some(7));
    }

    #[test]
    fn anything_else_is_no_pointer() {
        assert!(pointer(&format!("oid sha256:{}\nsize 1\n", OID.to_uppercase())).is_none(), "uppercase hex");
        assert!(pointer(&format!("oid sha256:{}\nsize 1\n", &OID[1..])).is_none(), "short oid");
        assert!(pointer(&format!("oid md5:{OID}\nsize 1\n")).is_none());
        assert!(pointer(&format!("oid sha256:{OID}\nsize -1\n")).is_none());
        assert!(pointer(&format!("oid sha256:{OID}\nsize \n")).is_none());
        assert!(pointer(&format!("oid sha256:{OID}\nsize 1\nextra 1\n")).is_none(), "a line after the size");
        assert!(pointer(&format!("size 1\noid sha256:{OID}\n")).is_none(), "out of order");
        assert!(parse_pointer(format!("version https://example.com/v1\noid sha256:{OID}\nsize 1\n").as_bytes()).is_none());
        let long = format!("{VERSION}\noid sha256:{OID}\nsize 1\n{}", " ".repeat(POINTER_LIMIT));
        assert!(parse_pointer(long.as_bytes()).is_none(), "1024 bytes or more");
        assert!(parse_pointer(b"\x89PNG\r\n\x1a\n").is_none());
    }

    #[test]
    fn extensions_are_read_in_order() {
        let p = pointer(&format!("ext-0-foo sha256:{OID}\next-1-bar sha256:{OID}\noid sha256:{OID}\nsize 3\n")).expect("a pointer");
        assert!(p.extensions);
        assert!(pointer(&format!("ext-1-bar sha256:{OID}\next-0-foo sha256:{OID}\noid sha256:{OID}\nsize 3\n")).is_none(), "out of order");
        assert!(pointer(&format!("ext-0-foo md5:{OID}\noid sha256:{OID}\nsize 3\n")).is_none());
        assert!(pointer(&format!("ext-x-foo sha256:{OID}\noid sha256:{OID}\nsize 3\n")).is_none());
    }
}
