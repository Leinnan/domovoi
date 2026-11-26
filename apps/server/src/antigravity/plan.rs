//! Antigravity's plans: markdown artifacts the agent writes in its session
//! `brain` folder (outside the workspace) and points to in its reply.
//!
//! Unlike Claude (`ExitPlanMode`) or Codex (`plan` items), Antigravity writes
//! plans through `write_file` / `edit_file` with `ArtifactMetadata` under
//! `<profile>/antigravity-acp/brain/<session_id>/*.md`, then refers to that
//! `file://` path in its final message without repeating the plan's text.
//! Reading the full markdown from the session's own brain folder captures the
//! plan untruncated (even past `protocol.rs`'s 8 KiB tool-text cap) so every
//! client can render and implement it as a `ProposedPlan`.

#![expect(clippy::disallowed_types, reason = "Antigravity's raw ACP tool-call JSON, inspected before truncation")]

use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Largest plan artifact read into a `ProposedPlan` (512 KiB).
pub const MAX_PLAN_BYTES: u64 = 512 * 1024;

/// What an un-truncated Antigravity tool update says about a possible plan artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSignal {
    pub path: Option<String>,
    pub request_feedback: bool,
    pub user_facing: Option<bool>,
    pub inline_markdown: Option<String>,
    pub is_edit_tool: bool,
}

impl PlanSignal {
    /// Merges a later `tool_call_update` onto what earlier updates already said
    /// (updates often omit `rawInput` once the call finishes).
    pub fn merge(&mut self, newer: PlanSignal) {
        if newer.path.is_some() {
            self.path = newer.path;
        }
        self.request_feedback |= newer.request_feedback;
        if newer.user_facing.is_some() {
            self.user_facing = newer.user_facing;
        }
        if newer.inline_markdown.is_some() {
            self.inline_markdown = newer.inline_markdown;
        }
        self.is_edit_tool |= newer.is_edit_tool;
    }
}

fn first_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v[*k].as_str()).map(str::trim).filter(|s| !s.is_empty())
}

fn bool_field(v: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|k| v[*k].as_bool())
}

fn is_edit_tool_name(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "write_to_file" | "replace_file_content" | "multi_replace_file_content"
    )
}

/// Extracts plan-artifact metadata and path from a raw `tool_call` or
/// `tool_call_update` BEFORE `normalize_update` bounds or drops long fields.
pub fn extract_signal(u: &Value) -> Option<PlanSignal> {
    if !matches!(u["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update")) {
        return None;
    }
    let raw = &u["rawInput"];
    let args = &u["args"];
    let meta = [&raw["ArtifactMetadata"], &raw["artifact_metadata"], &args["ArtifactMetadata"], &args["artifact_metadata"]]
        .into_iter()
        .find(|v| v.is_object());
    let kind_edit = u["kind"].as_str() == Some("edit");
    let name_edit = u["name"].as_str().is_some_and(is_edit_tool_name);
    let diff = u["content"].as_array().and_then(|c| c.iter().find(|b| b["type"] == "diff"));
    let is_edit_tool = kind_edit || name_edit || diff.is_some() || meta.is_some();

    let path = first_str(raw, &["Path", "path", "TargetFile", "target_file", "file_path"])
        .or_else(|| first_str(args, &["Path", "path", "TargetFile", "target_file", "file_path"]))
        .or_else(|| diff.and_then(|d| d["path"].as_str()).map(str::trim).filter(|s| !s.is_empty()))
        .or_else(|| u["locations"].as_array().and_then(|l| l.first()).and_then(|l| l["path"].as_str()).map(str::trim).filter(|s| !s.is_empty()))
        .map(str::to_owned);

    let request_feedback = meta.and_then(|m| bool_field(m, &["RequestFeedback", "request_feedback"])).unwrap_or(false);
    let user_facing = meta.and_then(|m| bool_field(m, &["UserFacing", "user_facing"]));

    let inline_markdown = first_str(raw, &["Content", "content", "CodeContent"])
        .or_else(|| first_str(args, &["Content", "content", "CodeContent"]))
        .or_else(|| diff.and_then(|d| d["newText"].as_str()).map(str::trim).filter(|s| !s.is_empty()))
        .filter(|text| !text.contains(super::protocol::TRUNCATED))
        .map(str::to_owned);

    if path.is_none() && meta.is_none() && inline_markdown.is_none() && !is_edit_tool {
        return None;
    }
    Some(PlanSignal { path, request_feedback, user_facing, inline_markdown, is_edit_tool })
}

/// Whether `path` has a `.md` extension and a file name indicating a plan.
pub fn is_plan_filename(path: &Path) -> bool {
    if !is_markdown_path(path) {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_ascii_lowercase) else {
        return false;
    };
    stem == "plan"
        || stem == "implementation_plan"
        || stem == "implementation-plan"
        || stem.ends_with("_plan")
        || stem.ends_with("-plan")
        || stem.starts_with("plan_")
        || stem.starts_with("plan-")
        || stem.contains("_plan_")
        || stem.contains("-plan-")
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

fn is_task_filename(path: &Path) -> bool {
    path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "task" | "tasks"))
}

/// Decides whether a markdown artifact inside the session's brain folder should
/// be captured as a `ProposedPlan`.
pub fn should_capture(path: &Path, signal: Option<&PlanSignal>, interaction_mode: &str, already_tracked: bool) -> bool {
    if !is_markdown_path(path) {
        return false;
    }
    if signal.and_then(|s| s.user_facing) == Some(false) {
        return false;
    }
    if signal.is_some_and(|s| s.request_feedback) || already_tracked || is_plan_filename(path) {
        return true;
    }
    interaction_mode == "plan" && !is_task_filename(path)
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Converts a plain path or `file://` URL to a local `PathBuf`.
pub fn parse_local_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("file://") {
        let url = url::Url::parse(trimmed).ok()?;
        if url.host_str().is_some_and(|h| !h.is_empty() && h != "localhost") {
            return None;
        }
        return url.to_file_path().ok();
    }
    if trimmed.contains("://") {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Rejects internal or scratch folders inside a brain session directory.
fn allowed_relative_artifact(rel: &Path) -> bool {
    let mut count = 0usize;
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_string_lossy();
                if s.starts_with('.') || (count == 0 && s.eq_ignore_ascii_case("scratch")) {
                    return false;
                }
                count += 1;
            }
            _ => return false,
        }
    }
    count > 0 && is_markdown_path(rel)
}

/// Walks `parts` below `root`, refusing any symlink along the way.
fn real_dir(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for part in parts {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).ok()?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return None;
        }
    }
    (std::fs::canonicalize(&current).ok()? == current).then_some(current)
}

/// Lexical check for an in-progress artifact path under `<profile>/antigravity-acp/brain/<session_id>/`
/// before the file may even exist on disk.
pub fn in_brain_scope(profile_home: &Path, session_id: &str, raw_path: &str) -> Option<PathBuf> {
    if !valid_session_id(session_id) {
        return None;
    }
    let path = parse_local_path(raw_path)?;
    if !path.is_absolute() {
        return None;
    }
    let home = std::fs::canonicalize(profile_home).unwrap_or_else(|_| profile_home.to_path_buf());
    let expected = home.join("antigravity-acp").join("brain").join(session_id);
    let candidate = match path.strip_prefix(&expected) {
        Ok(rel) => Some((expected.join(rel), rel.to_path_buf())),
        Err(_) => {
            let uncanon = profile_home.join("antigravity-acp").join("brain").join(session_id);
            path.strip_prefix(&uncanon).ok().map(|rel| (expected.join(rel), rel.to_path_buf()))
        }
    };
    let (joined, rel) = candidate?;
    allowed_relative_artifact(&rel).then_some(joined)
}

/// Opens a regular UTF-8 markdown file with symlink protection (`O_NOFOLLOW`) and size cap.
fn read_markdown_file(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || crate::util::is_link(path) || !meta.is_file() {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLAN_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return None;
    }
    // A plan written on Windows has CRLF line endings: shown alike everywhere.
    let text = String::from_utf8(bytes).ok()?.replace("\r\n", "\n");
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Reads a markdown artifact strictly inside `<profile_home>/antigravity-acp/brain/<session_id>/`.
pub fn read_brain_markdown(profile_home: &Path, session_id: &str, raw_path: &str) -> Option<(PathBuf, String)> {
    if !valid_session_id(session_id) {
        return None;
    }
    let root = std::fs::canonicalize(profile_home).ok()?;
    let brain = real_dir(&root, &["antigravity-acp", "brain", session_id])?;
    let path = parse_local_path(raw_path)?;
    if !path.is_absolute() {
        return None;
    }
    let canonical = std::fs::canonicalize(&path).ok()?;
    let rel = canonical.strip_prefix(&brain).ok()?;
    if !allowed_relative_artifact(rel) {
        return None;
    }
    let markdown = read_markdown_file(&canonical)?;
    Some((canonical, markdown))
}

/// Reads a markdown artifact from any real session folder under `<profile_home>/antigravity-acp/brain/`.
/// Used by startup repair for older threads whose session folder is named in the stored path.
pub fn read_profile_brain_markdown(profile_home: &Path, raw_path: &str) -> Option<(PathBuf, String)> {
    let root = std::fs::canonicalize(profile_home).ok()?;
    let brain_root = real_dir(&root, &["antigravity-acp", "brain"])?;
    let path = parse_local_path(raw_path)?;
    if !path.is_absolute() {
        return None;
    }
    let canonical = std::fs::canonicalize(&path).ok()?;
    let under_brain = canonical.strip_prefix(&brain_root).ok()?;
    let mut comps = under_brain.components();
    let Component::Normal(session) = comps.next()? else { return None };
    let session = session.to_str()?;
    if !valid_session_id(session) {
        return None;
    }
    // Verify the session directory itself is a real directory, not a symlink.
    let session_dir = real_dir(&brain_root, &[session])?;
    let rel = canonical.strip_prefix(&session_dir).ok()?;
    if !allowed_relative_artifact(rel) {
        return None;
    }
    let markdown = read_markdown_file(&canonical)?;
    Some((canonical, markdown))
}

/// Extracts local `.md` paths referenced via `file://` URLs in markdown text.
pub fn markdown_file_links(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = 0usize;
    while let Some(offset) = text[cursor..].find("file://") {
        let start = cursor + offset;
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | '`'))
            .unwrap_or(rest.len());
        let candidate = rest[..end].trim_end_matches(['.', ',', ';', ':']);
        cursor = start + end.max(7);
        if let Some(path) = parse_local_path(candidate)
            && path.is_absolute()
            && is_markdown_path(&path)
            && seen.insert(path.clone())
        {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_artifact_signal_from_raw_write_file_call() {
        let path = "/tmp/p/antigravity-acp/brain/s1/claude_artifacts_plan.md";
        let call = json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "name": "write_file",
            "kind": "edit",
            "locations": [{ "path": path }],
            "rawInput": {
                "Path": path,
                "Content": "# Full Plan\n1. Step one\n",
                "ArtifactMetadata": {
                    "RequestFeedback": true,
                    "UserFacing": true,
                    "Summary": "Implementation plan"
                }
            }
        });
        let sig = extract_signal(&call).expect("plan signal");
        assert_eq!(sig.path.as_deref(), Some(path));
        assert!(sig.request_feedback);
        assert_eq!(sig.user_facing, Some(true));
        assert_eq!(sig.inline_markdown.as_deref(), Some("# Full Plan\n1. Step one"));
        assert!(sig.is_edit_tool);
    }

    #[test]
    fn classifies_plan_artifacts_across_modes() {
        let req = PlanSignal { request_feedback: true, user_facing: Some(true), ..Default::default() };
        let non_user = PlanSignal { request_feedback: true, user_facing: Some(false), ..Default::default() };
        let plain = PlanSignal { request_feedback: false, user_facing: Some(true), ..Default::default() };

        assert!(should_capture(Path::new("claude_artifacts_plan.md"), Some(&req), "default", false));
        assert!(should_capture(Path::new("ios_note_view_analysis_and_plan.md"), None, "default", false));
        assert!(should_capture(Path::new("app_icon_plan.md"), None, "default", false));
        assert!(should_capture(Path::new("design.md"), Some(&req), "default", false));
        assert!(should_capture(Path::new("design.md"), Some(&plain), "plan", false));
        assert!(should_capture(Path::new("design.md"), None, "default", true));
        assert!(!should_capture(Path::new("task.md"), Some(&plain), "plan", false));
        assert!(!should_capture(Path::new("walkthrough.md"), Some(&plain), "default", false));
        assert!(!should_capture(Path::new("claude_artifacts_plan.md"), Some(&non_user), "plan", false));
        assert!(!should_capture(Path::new("notes.txt"), Some(&req), "plan", false));
    }

    #[test]
    fn reads_brain_markdown_strictly_inside_session_dir() {
        let root = std::env::temp_dir().join(format!("domovoi-ag-plan-{}", crate::util::new_uuid()));
        let brain = root.join("antigravity-acp/brain/sess-1");
        let scratch = brain.join("scratch");
        let sys = brain.join(".system_generated");
        let outside = root.join("outside");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let plan_file = brain.join("implementation_plan.md");
        std::fs::write(&plan_file, "# Ship it\n\n1. Do the thing\n").unwrap();
        let scratch_file = scratch.join("draft_plan.md");
        std::fs::write(&scratch_file, "# Scratch").unwrap();
        let sys_file = sys.join("plan.md");
        std::fs::write(&sys_file, "# Sys").unwrap();
        let outside_file = outside.join("plan.md");
        std::fs::write(&outside_file, "# Outside").unwrap();

        let (canon, md) = read_brain_markdown(&root, "sess-1", plan_file.to_str().unwrap()).expect("plan read");
        assert!(canon.ends_with("implementation_plan.md"));
        assert_eq!(md, "# Ship it\n\n1. Do the thing");

        let (canon2, md2) = read_profile_brain_markdown(&root, plan_file.to_str().unwrap()).expect("profile read");
        assert_eq!((canon2, md2), (canon, md));

        assert!(read_brain_markdown(&root, "sess-1", scratch_file.to_str().unwrap()).is_none());
        assert!(read_brain_markdown(&root, "sess-1", sys_file.to_str().unwrap()).is_none());
        assert!(read_brain_markdown(&root, "sess-1", outside_file.to_str().unwrap()).is_none());
        assert!(read_brain_markdown(&root, "other", plan_file.to_str().unwrap()).is_none());
        assert!(read_brain_markdown(&root, "../x", plan_file.to_str().unwrap()).is_none());

        #[cfg(unix)]
        {
            let link = brain.join("escape_plan.md");
            std::os::unix::fs::symlink(&outside_file, &link).unwrap();
            assert!(read_brain_markdown(&root, "sess-1", link.to_str().unwrap()).is_none());
            assert!(read_profile_brain_markdown(&root, link.to_str().unwrap()).is_none());
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    fn finds_windows_markdown_file_links() {
        let text = "See [plan.md](file:///C:/Users/test/.domovoi/brain/s1/plan.md).";
        assert_eq!(markdown_file_links(text), vec![PathBuf::from(r"C:\Users\test\.domovoi\brain\s1\plan.md")]);
    }

    #[test]
    #[cfg(unix)]
    fn finds_markdown_file_links_in_assistant_messages() {
        let text = "See [claude_artifacts_plan.md](file:///Users/test/.domovoi/brain/s1/claude_artifacts_plan.md) and [code](file:///w/main.rs).";
        let links = markdown_file_links(text);
        assert_eq!(links, vec![PathBuf::from("/Users/test/.domovoi/brain/s1/claude_artifacts_plan.md")]);
    }
}
