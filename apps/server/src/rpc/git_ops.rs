//! Git writes, review diffs, worktree setup progress and opening a folder in
//! an editor (t3code `GitWorkflowService`, `ReviewService`,
//! `WorktreeSetupTracker`, `externalLauncher`).

use super::Never;
use crate::state::ConnCtx;
use crate::vcs_ops::{self, DiffFileContentsInput, DiffPreviewInput, sent};
use crate::worktree_setup::Snapshot;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use domovoi_model::typed::EditorId;
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefInput {
    cwd: String,
    ref_name: String,
    #[serde(default)]
    switch_ref: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorktreeInput {
    cwd: String,
    ref_name: String,
    new_ref_name: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveWorktreeInput {
    cwd: String,
    path: String,
    #[serde(default)]
    force: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadOnly {
    thread_id: String,
}

/// `VcsInitInput`.
#[derive(Deserialize)]
struct InitInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    kind: Option<String>,
}

/// `VcsPullInput`.
#[derive(Deserialize)]
struct PullInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
}

/// What `vcs.refreshStatus` is asked with: a checkout, and whether to leave its upstream out.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshStatusInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::is_true")]
    local_only: bool,
}

/// `WorktreeSetupCancelResult`.
#[derive(Serialize)]
struct Cancelled {
    cancelled: bool,
}

/// `subscribeWorktreeSetup`: the thread's current setup (or null), then updates.
fn worktree_setup_stream(input: ThreadOnly) -> BoxStream<'static, Result<Option<Snapshot>, Never>> {
    let tracker = crate::worktree_setup::tracker();
    async_stream::stream! {
        let mut rx = tracker.subscribe();
        let current = tracker.current(&input.thread_id);
        let mut last = current.as_ref().map_or(0, |r| r.sequence);
        yield Ok(current);
        loop {
            match rx.recv().await {
                Ok((tid, record)) if tid == input.thread_id => {
                    let seq = record.sequence;
                    if seq > last || seq == 1 {
                        last = seq;
                        yield Ok(Some(record));
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(r) = tracker.current(&input.thread_id) {
                        last = r.sequence;
                        yield Ok(Some(r));
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

// ---- editors ----

#[derive(Clone, Copy, PartialEq)]
enum LaunchStyle {
    Goto,
    DirectPath,
    LineColumn,
}

/// t3code's `EDITORS`: CLI commands (first found wins), base args, launch style.
fn editor(id: &str) -> Option<(&'static [&'static str], &'static [&'static str], LaunchStyle)> {
    use LaunchStyle::*;
    Some(match id {
        "cursor" => (&["cursor"], &[], Goto),
        "trae" => (&["trae"], &[], Goto),
        "kiro" => (&["kiro"], &["ide"], Goto),
        "vscode" => (&["code"], &[], Goto),
        "vscode-insiders" => (&["code-insiders"], &[], Goto),
        "vscodium" => (&["codium"], &[], Goto),
        "zed" => (&["zed", "zeditor"], &[], DirectPath),
        "antigravity" => (&["antigravity-ide", "agy-ide"], &[], Goto),
        "idea" => (&["idea"], &[], LineColumn),
        "aqua" => (&["aqua"], &[], LineColumn),
        "clion" => (&["clion"], &[], LineColumn),
        "datagrip" => (&["datagrip"], &[], LineColumn),
        "dataspell" => (&["dataspell"], &[], LineColumn),
        "goland" => (&["goland"], &[], LineColumn),
        "phpstorm" => (&["phpstorm"], &[], LineColumn),
        "pycharm" => (&["pycharm"], &[], LineColumn),
        "rider" => (&["rider"], &[], LineColumn),
        "rubymine" => (&["rubymine"], &[], LineColumn),
        "rustrover" => (&["rustrover"], &[], LineColumn),
        "webstorm" => (&["webstorm"], &[], LineColumn),
        "file-manager" => (&[], &[], DirectPath),
        _ => return None,
    })
}

/// `path:line[:column]` targets.
fn split_position(target: &str) -> Option<(&str, &str, Option<&str>)> {
    let (rest, last) = target.rsplit_once(':')?;
    if !last.chars().all(|c| c.is_ascii_digit()) || last.is_empty() {
        return None;
    }
    match rest.rsplit_once(':') {
        Some((path, line)) if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) && !path.is_empty() => Some((path, line, Some(last))),
        _ if !rest.is_empty() => Some((rest, last, None)),
        _ => None,
    }
}

/// `resolveCommandEditorArgs`.
fn editor_args(style: LaunchStyle, target: &str, is_dir: bool, project_root: Option<&str>) -> Vec<String> {
    let positioned = split_position(target);
    match style {
        LaunchStyle::DirectPath => vec![target.into()],
        LaunchStyle::Goto if positioned.is_some() => vec!["--goto".into(), target.into()],
        LaunchStyle::Goto if is_dir => vec!["--new-window".into(), target.into()],
        LaunchStyle::Goto => vec![target.into()],
        LaunchStyle::LineColumn => match positioned {
            None => vec![target.into()],
            Some((path, line, column)) => {
                let mut a: Vec<String> = project_root.map(|r| vec![r.to_owned()]).unwrap_or_default();
                a.extend(["--line".into(), line.into()]);
                if let Some(c) = column {
                    a.extend(["--column".into(), (c.parse::<u64>().unwrap_or(1).saturating_sub(1)).to_string()]);
                }
                a.push(path.into());
                a
            }
        },
    }
}

/// Editors whose command is on PATH, plus the file manager (`availableEditors`).
pub fn available_editors() -> Vec<EditorId> {
    const IDS: [&str; 20] = ["cursor", "trae", "kiro", "vscode", "vscode-insiders", "vscodium", "zed", "antigravity", "idea", "aqua", "clion",
        "datagrip", "dataspell", "goland", "phpstorm", "pycharm", "rider", "rubymine", "rustrover", "webstorm"];
    let mut out: Vec<EditorId> = IDS.iter().filter(|id| editor(id).is_some_and(|(cmds, _, _)| cmds.iter().any(|c| find_command(c).is_some()))).map(|id| EditorId::from(*id)).collect();
    out.push(EditorId::from("file-manager"));
    out
}

fn find_command(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|p| p.join(name)).find(|p| p.is_file()))
}

/// What `shell.openInEditor` is asked with (`OpenInEditorInput`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenInEditorInput {
    /// The folder or file to open; a file may end in `:line` or `:line:column`.
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::text")]
    editor: String,
    /// For the file manager: select the target in its folder.
    #[serde(default, deserialize_with = "sent::is_true")]
    reveal: bool,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    project_root: Option<String>,
}

/// Why nothing was opened: t3code's `ExternalLauncherError`s. (The one for a
/// launch that did not start says no `target`, which t3code's does.)
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
enum LauncherFailure {
    #[error("Unknown editor: {editor}")]
    #[serde(rename = "ExternalLauncherUnknownEditorError")]
    UnknownEditor { editor: String },
    /// No file manager can be started here.
    #[error("Unsupported editor: {editor}")]
    #[serde(rename = "ExternalLauncherUnsupportedEditorError")]
    UnsupportedEditor { editor: String },
    #[error("Editor command not found: {command}")]
    #[serde(rename = "ExternalLauncherCommandNotFoundError")]
    CommandNotFound { editor: String, command: &'static str },
    #[error("Failed to launch {editor} with '{command}': {cause}")]
    #[serde(rename = "ExternalLauncherEditorSpawnError")]
    EditorSpawn { editor: String, command: String, args: Vec<String>, cause: String },
}

async fn open_in_editor(_ctx: Arc<ConnCtx>, input: OpenInEditorInput) -> Result<(), LauncherFailure> {
    let id = input.editor.as_str();
    let (commands, base_args, style) = editor(id).ok_or_else(|| LauncherFailure::UnknownEditor { editor: id.to_owned() })?;
    let target = crate::util::expand_home(&input.cwd);
    let (command, args): (String, Vec<String>) = if commands.is_empty() {
        // The file manager: Finder on macOS, xdg-open elsewhere.
        if cfg!(target_os = "macos") {
            let args = if input.reveal { vec!["-R".into(), target] } else { vec![target] };
            ("open".into(), args)
        } else if find_command("xdg-open").is_some() && std::env::var_os("DISPLAY").or(std::env::var_os("WAYLAND_DISPLAY")).is_some() {
            let dir = if input.reveal { std::path::Path::new(&target).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or(target.clone()) } else { target };
            ("xdg-open".into(), vec![dir])
        } else {
            return Err(LauncherFailure::UnsupportedEditor { editor: id.to_owned() });
        }
    } else {
        let found = commands.iter().find_map(|c| find_command(c));
        let Some(cmd) = found else {
            return Err(LauncherFailure::CommandNotFound { editor: id.to_owned(), command: commands[0] });
        };
        let is_dir = style == LaunchStyle::Goto && std::path::Path::new(&target).is_dir();
        let mut args: Vec<String> = base_args.iter().map(|s| s.to_string()).collect();
        args.extend(editor_args(style, &target, is_dir, input.project_root.as_deref()));
        (cmd.to_string_lossy().into_owned(), args)
    };
    crate::process::blocking_command(&command)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| LauncherFailure::EditorSpawn { editor: id.to_owned(), command, args, cause: e.to_string() })
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("vcs.createRef", |_ctx: Arc<ConnCtx>, i: RefInput| async move { vcs_ops::create_ref(&i.cwd, &i.ref_name, i.switch_ref.unwrap_or(false)).await })
        .unary("vcs.switchRef", |_ctx: Arc<ConnCtx>, i: RefInput| async move { vcs_ops::switch_ref(&i.cwd, &i.ref_name).await })
        .unary("vcs.createWorktree", |_ctx: Arc<ConnCtx>, i: CreateWorktreeInput| async move {
            vcs_ops::create_worktree(&i.cwd, &i.ref_name, i.new_ref_name.as_deref(), i.path.as_deref()).await
        })
        .unary("vcs.removeWorktree", |_ctx: Arc<ConnCtx>, i: RemoveWorktreeInput| async move {
            vcs_ops::remove_worktree(&i.cwd, &i.path, i.force.unwrap_or(false)).await
        })
        .unary("vcs.init", |_ctx: Arc<ConnCtx>, i: InitInput| async move { vcs_ops::init(&i.cwd, i.kind.as_deref()).await })
        .unary("vcs.pull", |_ctx: Arc<ConnCtx>, i: PullInput| async move { vcs_ops::pull(&i.cwd).await })
        .unary("vcs.refreshStatus", |_ctx: Arc<ConnCtx>, i: RefreshStatusInput| async move {
            Ok::<_, Never>(vcs_ops::refresh_status(&i.cwd, i.local_only).await)
        })
        .unary("review.getDiffPreview", |_ctx: Arc<ConnCtx>, i: DiffPreviewInput| async move { vcs_ops::diff_preview(&i).await })
        .unary("review.getDiffFileContents", |_ctx: Arc<ConnCtx>, i: DiffFileContentsInput| async move { vcs_ops::diff_file_contents(&i).await })
        .stream("subscribeWorktreeSetup", |_ctx: Arc<ConnCtx>, i: ThreadOnly| worktree_setup_stream(i))
        .unary("worktreeSetup.cancel", |_ctx: Arc<ConnCtx>, i: ThreadOnly| async move {
            Ok::<_, Never>(Cancelled { cancelled: crate::worktree_setup::tracker().cancel(&i.thread_id) })
        })
        .unary("shell.openInEditor", open_in_editor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_arguments_follow_t3code() {
        assert_eq!(editor_args(LaunchStyle::Goto, "/p/a.rs:3:4", false, None), ["--goto", "/p/a.rs:3:4"]);
        assert_eq!(editor_args(LaunchStyle::Goto, "/p", true, None), ["--new-window", "/p"]);
        assert_eq!(editor_args(LaunchStyle::LineColumn, "/p/a.rs:3:4", false, Some("/p")), ["/p", "--line", "3", "--column", "3", "/p/a.rs"]);
        assert_eq!(editor_args(LaunchStyle::LineColumn, "/p/a.rs:7", false, None), ["--line", "7", "/p/a.rs"]);
        assert_eq!(editor_args(LaunchStyle::DirectPath, "/p/a.rs:7", false, None), ["/p/a.rs:7"]);
        assert!(editor("notepad").is_none());
    }

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        use serde_json::json;
        assert_eq!(json!(LauncherFailure::UnknownEditor { editor: "notepad".into() }), json!({ "_tag": "ExternalLauncherUnknownEditorError", "editor": "notepad" }));
        assert_eq!(
            json!(LauncherFailure::UnsupportedEditor { editor: "file-manager".into() }),
            json!({ "_tag": "ExternalLauncherUnsupportedEditorError", "editor": "file-manager" })
        );
        assert_eq!(
            json!(LauncherFailure::CommandNotFound { editor: "zed".into(), command: "zed" }),
            json!({ "_tag": "ExternalLauncherCommandNotFoundError", "editor": "zed", "command": "zed" })
        );
        assert_eq!(
            json!(LauncherFailure::EditorSpawn { editor: "vscode".into(), command: "/bin/code".into(), args: vec!["--goto".into(), "/p/a.rs:3".into()], cause: "Permission denied (os error 13)".into() }),
            json!({ "_tag": "ExternalLauncherEditorSpawnError", "editor": "vscode", "command": "/bin/code", "args": ["--goto", "/p/a.rs:3"], "cause": "Permission denied (os error 13)" })
        );
    }
}
