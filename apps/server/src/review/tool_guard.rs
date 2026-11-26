//! Host-native read-only tool guard for isolated AI review execution.
//!
//! Invoked via Claude's `PreToolUse` hook (`domovoi-server review-tool-guard --root <dir>`)
//! to enforce fail-closed read-only access strictly rooted inside the frozen snapshot
//! directory, without requiring Docker.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Claude PreToolUse hook JSON protocol on stdin/stdout")]

use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};

const DENIED: &str = "Frozen reviews allow only read-only inspection inside the snapshot directory.";

/// Generates Claude `--settings` JSON wiring `PreToolUse` to this binary's `review-tool-guard`.
pub fn claude_guard_settings_json(snapshot_root: &Path) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let cmd = format!("'{}' review-tool-guard --root '{}'", shell_escape(&exe.to_string_lossy()), shell_escape(&snapshot_root.to_string_lossy()));
    Some(
        json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": ".*",
                        "hooks": [{ "type": "command", "command": cmd }]
                    }
                ]
            }
        })
        .to_string(),
    )
}

fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// CLI entrypoint for `domovoi-server review-tool-guard --root <dir>`.
/// Returns exit code `0` to allow the tool call or `2` (blocking error) to deny it.
pub fn run_cli_guard(root: &Path) -> i32 {
    let mut raw = String::new();
    if std::io::stdin().take(256 * 1024).read_to_string(&mut raw).is_err() {
        eprintln!("{DENIED}");
        return 2;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
        eprintln!("{DENIED}");
        return 2;
    };
    let tool_name = payload["tool_name"].as_str().unwrap_or_default();
    let tool_input = &payload["tool_input"];
    match validate_tool_call(tool_name, tool_input, root) {
        Ok(()) => 0,
        Err(reason) => {
            eprintln!("{reason}");
            2
        }
    }
}

/// Fail-closed validation of a single tool call against `root`.
pub fn validate_tool_call(tool_name: &str, input: &Value, root: &Path) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(root).map_err(|_| DENIED.to_owned())?;
    match tool_name {
        "Read" => {
            let p = input["file_path"].as_str().ok_or(DENIED)?;
            ensure_under_root(Path::new(p), &canonical_root)
        }
        "Grep" | "Glob" => {
            if let Some(p) = input["path"].as_str() {
                ensure_under_root(Path::new(p), &canonical_root)?;
            }
            Ok(())
        }
        "Bash" => validate_bash_input(input, &canonical_root),
        _ => Err(format!("{DENIED} Tool `{tool_name}` is not permitted.")),
    }
}

fn ensure_under_root(candidate: &Path, canonical_root: &Path) -> Result<(), String> {
    let joined: PathBuf = if candidate.is_absolute() { candidate.to_path_buf() } else { canonical_root.join(candidate) };
    let resolved = std::fs::canonicalize(&joined).map_err(|_| DENIED.to_owned())?;
    if !resolved.starts_with(canonical_root) {
        return Err(format!("{DENIED} Path escapes snapshot root."));
    }
    Ok(())
}

fn validate_bash_input(input: &Value, canonical_root: &Path) -> Result<(), String> {
    let obj = input.as_object().ok_or(DENIED)?;
    for key in obj.keys() {
        if !matches!(key.as_str(), "command" | "description" | "timeout" | "run_in_background" | "dangerouslyDisableSandbox") {
            return Err(DENIED.into());
        }
    }
    if obj.get("run_in_background").is_some_and(|v| v.as_bool() == Some(true))
        || obj.get("dangerouslyDisableSandbox").is_some_and(|v| v.as_bool() == Some(true))
    {
        return Err(DENIED.into());
    }
    if let Some(t) = obj.get("timeout").and_then(Value::as_u64)
        && !(1..=30_000).contains(&t)
    {
        return Err(DENIED.into());
    }
    let command = obj.get("command").and_then(Value::as_str).ok_or(DENIED)?;
    validate_bash_command(command, canonical_root)
}

pub fn validate_bash_command(command: &str, canonical_root: &Path) -> Result<(), String> {
    if command.is_empty() || command.len() > 4096 || command.chars().any(char::is_control) {
        return Err(DENIED.into());
    }
    // Reject shell chaining, command substitution, redirects, and variable expansion.
    for forbidden in [";", "&&", "||", "$(", "`", ">", "<", "\n", "\r"] {
        if command.contains(forbidden) {
            return Err(DENIED.into());
        }
    }

    if let Some(args) = command.strip_prefix("sed -n ").or_else(|| command.strip_prefix("/usr/bin/sed -n ")) {
        let (range, rest) = take_single_quoted(args)?;
        let (start_s, end_s) = range.strip_suffix('p').and_then(|r| r.split_once(',')).ok_or(DENIED)?;
        let start: u32 = start_s.parse().map_err(|_| DENIED)?;
        let end: u32 = end_s.parse().map_err(|_| DENIED)?;
        if start == 0 || end < start || end - start > 200 || end > 1_000_000 {
            return Err(DENIED.into());
        }
        let rest = rest.strip_prefix(" -- ").ok_or(DENIED)?;
        let (path, tail) = take_single_quoted(rest)?;
        if !tail.is_empty() {
            return Err(DENIED.into());
        }
        return ensure_under_root(Path::new(&path), canonical_root);
    }

    if let Some(args) = command.strip_prefix("grep -r -n -F -- ").or_else(|| command.strip_prefix("/usr/bin/grep -r -n -F -- ")) {
        let (query, rest) = take_single_quoted(args)?;
        if query.is_empty() || query.len() > 256 {
            return Err(DENIED.into());
        }
        let rest = rest.strip_prefix(' ').ok_or(DENIED)?;
        let (path, tail) = take_single_quoted(rest)?;
        let limit_s = tail
            .strip_prefix(" | head -n ")
            .or_else(|| tail.strip_prefix(" | /usr/bin/head -n "))
            .ok_or(DENIED)?;
        let limit: u32 = limit_s.parse().map_err(|_| DENIED)?;
        if !(1..=80).contains(&limit) {
            return Err(DENIED.into());
        }
        return ensure_under_root(Path::new(&path), canonical_root);
    }

    if let Some(args) = command.strip_prefix("ls -1A -- ").or_else(|| command.strip_prefix("/usr/bin/ls -1A -- ")) {
        let (path, tail) = take_single_quoted(args)?;
        if !tail.is_empty() {
            return Err(DENIED.into());
        }
        return ensure_under_root(Path::new(&path), canonical_root);
    }

    Err(DENIED.into())
}

fn take_single_quoted(input: &str) -> Result<(String, &str), String> {
    let mut rest = input.strip_prefix('\'').ok_or(DENIED)?;
    let mut out = String::new();
    loop {
        let end = rest.find('\'').ok_or(DENIED)?;
        out.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        if let Some(next) = rest.strip_prefix("\\''") {
            out.push('\'');
            rest = next;
        } else {
            return Ok((out, rest));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_bounded_snapshot_reads_and_blocks_escapes() {
        let root = std::env::temp_dir().join(format!("domovoi-guard-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(root.join("CHANGED")).unwrap();
        let file = root.join("CHANGED/a.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        assert!(validate_tool_call("Read", &json!({ "file_path": file }), &root).is_ok());
        assert!(validate_tool_call("Read", &json!({ "file_path": "/etc/passwd" }), &root).is_err());
        assert!(validate_tool_call("Write", &json!({ "file_path": file }), &root).is_err());
        assert!(validate_tool_call("Edit", &json!({ "file_path": file }), &root).is_err());

        let sed_ok = format!("sed -n '1,120p' -- '{}'", file.display());
        assert!(validate_tool_call("Bash", &json!({ "command": sed_ok }), &root).is_ok());

        let sed_too_wide = format!("sed -n '1,500p' -- '{}'", file.display());
        assert!(validate_tool_call("Bash", &json!({ "command": sed_too_wide }), &root).is_err());

        let grep_ok = format!("grep -r -n -F -- 'main' '{}' | head -n 40", root.display());
        assert!(validate_tool_call("Bash", &json!({ "command": grep_ok }), &root).is_ok());

        let chained = format!("ls -1A -- '{}' && rm -rf /", root.display());
        assert!(validate_tool_call("Bash", &json!({ "command": chained }), &root).is_err());

        assert!(
            validate_tool_call("Bash", &json!({ "command": format!("ls -1A -- '{}'", root.display()), "dangerouslyDisableSandbox": true }), &root).is_err()
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
