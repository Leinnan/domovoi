//! Server-managed project folders (t3code `ManagedProjectFolders`): the
//! "No project" scratch project with one folder per thread, and new named
//! projects scaffolded as git repositories under `<home>/projects`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `<home>`, set once at startup.
pub static HOME: OnceLock<PathBuf> = OnceLock::new();

const ICON_BACKGROUNDS: [&str; 15] = [
    "#dc2626", "#ea580c", "#d97706", "#16a34a", "#059669", "#0d9488", "#0891b2", "#0284c7", "#2563eb", "#4f46e5",
    "#7c3aed", "#9333ea", "#c026d3", "#db2777", "#e11d48",
];
const MAX_NAMED_FOLDER_ATTEMPTS: usize = 100;

/// The scratch root, unless the home sits inside a git checkout (scratch
/// folders would inherit its status and checkpoints).
pub fn scratch_root() -> Option<PathBuf> {
    let home = HOME.get()?;
    let inside_repo = crate::process::output_blocking(
        crate::process::blocking_command("git").args(["rev-parse", "--show-toplevel"]).current_dir(home),
        crate::process::QUICK,
    )
    .is_ok_and(|o| o.status.success());
    (!inside_repo).then(|| home.join("scratch"))
}

pub fn named_projects_root() -> Option<PathBuf> {
    HOME.get().map(|h| h.join("projects"))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn is_scratch_root(workspace_root: &str) -> bool {
    scratch_root().is_some_and(|r| same_dir(Path::new(workspace_root), &r))
}

/// Whether `folder` is a thread folder directly inside the scratch root.
pub fn is_scratch_thread_folder(folder: &str) -> bool {
    let Some(root) = scratch_root() else { return false };
    Path::new(folder).parent().is_some_and(|p| same_dir(p, &root))
}

/// `claimFreeFolder`: the first candidate this call creates.
fn claim(candidates: impl Iterator<Item = PathBuf>) -> std::io::Result<Option<PathBuf>> {
    for c in candidates {
        match std::fs::create_dir(&c) {
            Ok(()) => return Ok(Some(c)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// `folderWords`: up to five lowercase ASCII words, at most 48 characters.
pub fn folder_words(text: &str) -> String {
    let joined = text.to_lowercase().split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit()).filter(|w| !w.is_empty()).take(5).collect::<Vec<_>>().join("-");
    joined.chars().take(48).collect::<String>().trim_end_matches('-').to_owned()
}

/// `folderForThread`: `<scratch>/<date>-<words>-<id tail>` for a thread in the scratch project.
pub fn folder_for_thread(project_root: &str, thread_id: &str, text: &str) -> std::io::Result<Option<String>> {
    let Some(root) = scratch_root().filter(|r| same_dir(Path::new(project_root), r)) else { return Ok(None) };
    std::fs::create_dir_all(&root)?;
    let date = &domovoi_contracts::iso_now()[..10];
    let words = folder_words(text);
    let id: String = thread_id.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let folder_for = |id_part: &str| root.join([date, &words, id_part].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("-"));
    let tail = &id[id.len().saturating_sub(8)..];
    let candidates = [folder_for(tail), folder_for(&id)]
        .into_iter()
        .chain(std::iter::repeat_with(|| folder_for(&format!("{id}-{}", &crate::util::new_uuid()[..8]))).take(10));
    Ok(claim(candidates)?.map(|p| p.to_string_lossy().into_owned()))
}

/// `newProjectFolderName`: an ASCII slug of at most 64 characters.
pub fn project_folder_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let ascii: String = name.nfkd().filter(|c| !('\u{300}'..='\u{36f}').contains(c)).collect::<String>().to_lowercase();
    let mut slug = String::new();
    for c in ascii.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_start_matches('-').chars().take(64).collect::<String>().trim_end_matches('-').to_owned();
    if slug.is_empty() {
        return "project".into();
    }
    let reserved = ["con", "prn", "aux", "nul"].contains(&slug.as_str())
        || ((slug.starts_with("com") || slug.starts_with("lpt")) && slug.len() == 4 && slug.as_bytes()[3].is_ascii_digit() && slug.as_bytes()[3] != b'0');
    if reserved { format!("{slug}-project") } else { slug }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `namedProjectIconSvg`: initials on a color picked by the name's hash.
pub fn icon_svg(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut initials: String = words.iter().take(2).filter_map(|w| w.chars().next()).collect::<String>().to_uppercase();
    if initials.is_empty() {
        initials = name.trim().chars().next().map(String::from).unwrap_or_else(|| "?".into());
    }
    let mut hash: u32 = 0;
    for c in name.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(c as u32);
    }
    let background = ICON_BACKGROUNDS[(hash as usize) % ICON_BACKGROUNDS.len()];
    let size = if initials.chars().count() > 1 { 26 } else { 32 };
    [
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"#.to_owned(),
        format!(r##"  <rect width="64" height="64" rx="14" fill="{background}"/>"##),
        format!(
            r##"  <text x="32" y="32" dy="0.35em" text-anchor="middle" font-family="ui-sans-serif, system-ui, -apple-system, sans-serif" font-size="{size}" font-weight="600" fill="#ffffff">{}</text>"##,
            escape_xml(&initials)
        ),
        "</svg>".into(),
        String::new(),
    ]
    .join("\n")
}

fn readme(name: &str) -> String {
    format!("<img src=\"assets/icon.svg\" width=\"64\" height=\"64\" alt=\"\">\n\n# {name}\n\nCreated in [T3 Code](https://t3.codes).\n")
}

fn describe_commit_failure(stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    if ["identity unknown", "tell me who you are", "no name was given", "no email was given"].iter().any(|p| lower.contains(p)) {
        return "Git has no name or email on this machine. Set user.name and user.email, then commit.".into();
    }
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git commit failed.");
    line.to_owned()
}

/// Creates `<home>/projects/<slug>` as a git repository with a README and an
/// icon. Returns the folder and, when the initial commit failed, why.
pub fn scaffold_named_project(name: &str) -> Result<(String, Option<String>), String> {
    let root = named_projects_root().ok_or("The server home is not set.")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let slug = project_folder_name(name);
    let candidates = (1..=MAX_NAMED_FOLDER_ATTEMPTS).map(|n| root.join(if n == 1 { slug.clone() } else { format!("{slug}-{n}") }));
    let folder = claim(candidates).map_err(|e| e.to_string())?.ok_or_else(|| format!("Every folder name for \"{slug}\" is taken."))?;
    let git = |args: &[&str]| crate::process::output_blocking(crate::process::blocking_command("git").args(args).current_dir(&folder), crate::process::QUICK);
    let scaffold = || -> Result<Option<String>, String> {
        let branch = git(&["config", "--get", "init.defaultBranch"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "main".into());
        let init = git(&["init", &format!("--initial-branch={branch}")]).map_err(|e| e.to_string())?;
        if !init.status.success() {
            return Err(String::from_utf8_lossy(&init.stderr).trim().to_owned());
        }
        std::fs::write(folder.join("README.md"), readme(name)).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(folder.join("assets")).map_err(|e| e.to_string())?;
        std::fs::write(folder.join("assets/icon.svg"), icon_svg(name)).map_err(|e| e.to_string())?;
        git(&["-c", "core.safecrlf=false", "add", "--force", "--", "README.md", "assets/icon.svg"]).map_err(|e| e.to_string())?;
        Ok(match git(&["commit", "--message", "Initial commit"]) {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(describe_commit_failure(&String::from_utf8_lossy(&o.stderr))),
            Err(e) => Some(e.to_string()),
        })
    };
    match scaffold() {
        Ok(commit_error) => Ok((folder.to_string_lossy().into_owned(), commit_error)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_t3code() {
        assert_eq!(folder_words("Fix the Login bug, please now and later!"), "fix-the-login-bug-please");
        assert_eq!(project_folder_name("  Café Été  "), "cafe-ete");
        assert_eq!(project_folder_name("!!!"), "project");
        assert_eq!(project_folder_name("con"), "con-project");
        assert_eq!(project_folder_name("COM1"), "com1-project");
        let svg = icon_svg("my app");
        assert!(svg.contains(">MA</text>") && svg.contains("font-size=\"26\""));
        assert!(icon_svg("<x>").contains("&lt;") || icon_svg("<x>").contains(">X<"));
    }
}
