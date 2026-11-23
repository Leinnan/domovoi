//! Artifact tools (`t3_artifact_*`): create, update, list, and read durable,
//! versioned visual/interactive artifacts (HTML, React/JSX, SVG, Mermaid,
//! Markdown, code) directly in Domovoi's UI across all providers and plans.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, n, one_of, s, tool};
use crate::artifacts::{UpsertInput, list_for_thread, read_version, upsert};
use serde_json::{Value, json};
use std::path::Path;

const KINDS: &[&str] = &["html", "react", "svg", "mermaid", "markdown", "code"];

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "t3_artifact_create",
            "Create a standalone, previewable artifact (HTML web page/dashboard, interactive React JSX/TSX component, SVG illustration, Mermaid diagram, Markdown report, or code snippet) in the user's Artifacts panel. Prefer this over pasting large HTML/JSX/SVG blocks into chat.",
            json!({
                "id": s("Optional stable slug/id to identify this artifact across updates."),
                "title": s("Short human-readable title shown on the Artifact card and panel header."),
                "kind": one_of(KINDS, "Artifact type: html, react, svg, mermaid, markdown, or code."),
                "content": s("The full artifact source. Omit if `path` is provided to load a workspace file."),
                "path": s("Optional relative path in the thread's workspace to load content from (and enable multi-file preview for HTML)."),
                "language": s("Optional syntax language (e.g. tsx, html, svg, python)."),
                "summary": s("Optional brief note describing this version."),
            }),
            &["title", "kind"],
            false,
        ),
        tool(
            "t3_artifact_update",
            "Publish a new version of an existing artifact created in this thread, updating the user's Artifacts panel in place while keeping prior versions in history.",
            json!({
                "id": s("The artifact id returned by `t3_artifact_create`."),
                "content": s("Updated full artifact source. Omit if `path` is provided."),
                "path": s("Optional relative workspace file path to read the updated content from."),
                "title": s("Optional updated title."),
                "kind": one_of(KINDS, "Optional updated kind."),
                "language": s("Optional updated language."),
                "summary": s("Brief description of what changed in this version."),
            }),
            &["id"],
            false,
        ),
        tool(
            "t3_artifact_list",
            "List artifacts created in this thread, with their titles, kinds, and latest version numbers.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "t3_artifact_read",
            "Read an artifact's source and version history (latest version by default, or a specific `version`).",
            json!({
                "id": s("The artifact id."),
                "version": n("Optional 1-based version number (default: latest)."),
            }),
            &["id"],
            true,
        ),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "t3_artifact_create" => create_or_update(c, false),
        "t3_artifact_update" => create_or_update(c, true),
        "t3_artifact_list" => list(c),
        "t3_artifact_read" => read(c),
        _ => return None,
    })
}

fn load_workspace_file(cwd: &str, rel_path: &str) -> Result<(String, String), String> {
    let root = std::fs::canonicalize(cwd).map_err(|e| format!("Workspace not available: {e}"))?;
    let target = root.join(rel_path);
    let resolved = std::fs::canonicalize(&target).map_err(|e| format!("Could not find `{rel_path}`: {e}"))?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err(format!("`{rel_path}` must be a file inside the thread workspace."));
    }
    let rel = resolved
        .strip_prefix(&root)
        .ok()
        .and_then(Path::to_str)
        .unwrap_or(rel_path)
        .replace('\\', "/");
    let bytes = std::fs::read(&resolved).map_err(|e| format!("Could not read `{rel_path}`: {e}"))?;
    if bytes.len() > crate::artifacts::MAX_CONTENT_BYTES {
        return Err("File is too large for an artifact (max 2 MiB).".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("`{rel_path}` is not UTF-8 text."))?;
    Ok((rel, text))
}

fn thread_cwd(c: &Call<'_>) -> Option<String> {
    c.caller
        .thread
        .worktree_path
        .clone()
        .or_else(|| c.project_folder())
}

fn create_or_update(c: &Call<'_>, updating: bool) -> Result<Value, String> {
    let id = if updating {
        Some(c.required("id")?)
    } else {
        c.str("id")
    };
    let existing = match &id {
        Some(id) => read_version(c.store(), id, None)?,
        None => None,
    };
    if updating && existing.is_none() {
        return Err(format!("No artifact `{}` in this thread.", id.as_deref().unwrap_or_default()));
    }

    let path_arg = c.str("path").or_else(|| existing.as_ref().and_then(|d| d.artifact.source_path.clone()));
    let (resolved_path, content) = match (c.args["content"].as_str(), path_arg.as_deref()) {
        (Some(text), path) => (path.map(str::to_owned), text.to_owned()),
        (None, Some(path)) => {
            let cwd = thread_cwd(c).ok_or("No workspace folder for this thread.")?;
            let (rel, text) = load_workspace_file(&cwd, path)?;
            (Some(rel), text)
        }
        (None, None) => return Err("Provide `content` or a workspace `path`.".into()),
    };

    let title = c
        .str("title")
        .or_else(|| existing.as_ref().map(|d| d.artifact.title.clone()))
        .ok_or("title is required.")?;
    let kind = c
        .str("kind")
        .or_else(|| existing.as_ref().map(|d| d.artifact.kind.clone()))
        .ok_or("kind is required.")?;
    let language = c
        .str("language")
        .or_else(|| existing.as_ref().and_then(|d| d.artifact.language.clone()));
    let summary = c.str("summary");

    let (artifact, version) = upsert(
        c.store(),
        UpsertInput {
            id: id.as_deref(),
            thread_id: &c.caller.thread_id,
            project_id: &c.caller.project_id,
            title: &title,
            kind: &kind,
            language: language.as_deref(),
            source_path: resolved_path.as_deref(),
            hosted_url: None,
            run_id: None,
            turn_item_id: None,
            summary: summary.as_deref(),
            content: &content,
        },
    )?;

    Ok(json!({
        "artifactId": artifact.id,
        "title": artifact.title,
        "kind": artifact.kind,
        "language": artifact.language,
        "sourcePath": artifact.source_path,
        "version": version,
        "byteLength": content.len(),
    }))
}

fn list(c: &Call<'_>) -> Result<Value, String> {
    let items = list_for_thread(c.store(), &c.caller.thread_id)?;
    Ok(json!({ "artifacts": items }))
}

fn read(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("id")?;
    let version = c.args["version"].as_u64().map(|v| v as u32);
    let detail = read_version(c.store(), &id, version)?
        .filter(|d| d.artifact.thread_id == c.caller.thread_id)
        .ok_or_else(|| format!("Artifact `{id}` not found in this thread."))?;
    serde_json::to_value(detail).map_err(|e| e.to_string())
}
