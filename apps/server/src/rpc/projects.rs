//! Project files and managed projects: file tree, `@` mention search, content
//! search, ignored files, writes, file metadata, attachments, and the scratch
//! / named / promoted project flows (t3code `WorkspaceEntries`,
//! `WorkspaceFileSystem`, `ManagedProjectFolders`, `ScratchThreadPromotion`).

use super::Never;
use super::shell::{RecordOrNot, text_or_empty, those_that_fit};
use crate::attachments::{self, PersistRequest, UploadRequest};
use crate::state::ConnCtx;
use crate::workspace::{self, ContentQuery, EntriesError, EntryMetadata, FileFailure};
use domovoi_rpc::Router;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use crate::wire::shell::ProjectShell;
use domovoi_model::typed::command::ThreadProjectAdoptFolder;
use domovoi_model::typed::{
    AppThread, ProjectCreateFromThreadResult, ProjectCreateNewResult, ProjectEnsureScratchResult, ProjectEntryKind,
    ProjectId, RepositoryIdentity, Run, RuntimeRequest,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEntriesInput {
    cwd: String,
    directory_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchEntriesInput {
    cwd: String,
    query: String,
    limit: usize,
    kind: Option<ProjectEntryKind>,
    #[serde(default)]
    image_only: Option<bool>,
}

/// `ProjectListIgnoredFilesInput`. A `cwd` left out, or one that is no text,
/// reads as an empty one, which no workspace has.
#[derive(Deserialize, Default)]
struct ListIgnoredFilesInput {
    #[serde(default, deserialize_with = "text_or_empty")]
    cwd: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchContentsInput {
    cwd: String,
    query: String,
    limit: usize,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileInput {
    cwd: String,
    relative_path: String,
    contents: String,
}

/// `ProjectWriteFileResult`: the path written, with `/` between its parts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileResult {
    relative_path: String,
}

/// `FilesystemGetMetadataInput`. A path that is no text is left out, and
/// anything but a list of paths reads as none.
#[derive(Deserialize, Default)]
struct GetMetadataInput {
    #[serde(default, deserialize_with = "those_that_fit")]
    paths: Vec<String>,
}

/// `FilesystemGetMetadataResult`: the paths asked about, in that order; `null`
/// for one that is missing, relative or the server's own.
#[derive(Serialize)]
struct GetMetadataResult {
    entries: Vec<Option<EntryMetadata>>,
}

/// `AttachmentDeleteInput`. An id left out, or one that is no text, names nothing.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeleteAttachmentInput {
    #[serde(default, deserialize_with = "text_or_empty")]
    attachment_id: String,
}

/// `ProjectCreateFromThreadInput`. An id left out, or one that is no text,
/// names no thread.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateFromThreadInput {
    #[serde(default, deserialize_with = "text_or_empty")]
    thread_id: String,
}

/// Why a managed project was not made, told as t3code's
/// `OrchestrationDispatchCommandError`: the one error these calls declare.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub(crate) enum Failure {
    #[error("{message}")]
    #[serde(rename = "OrchestrationDispatchCommandError")]
    Dispatch { message: String },
}

/// Runs blocking filesystem work off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("workspace task panicked")
}

fn dispatch_error(message: impl Into<String>) -> Failure {
    Failure::Dispatch { message: message.into() }
}

/// A project as it is when made: nothing chosen yet. `repository_identity` is
/// what `vcs::repository_identity` answered for the folder.
pub(crate) fn new_project_shell(id: String, title: String, workspace_root: String, repository_identity: Option<RepositoryIdentity>) -> ProjectShell {
    let now = crate::time_fmt::iso_now();
    ProjectShell {
        id,
        title,
        has_custom_title: false,
        workspace_root,
        repository_identity,
        default_model_selection: None,
        default_thread_env_mode: None,
        auto_pull: false,
        favicon_path: None,
        project_icon: None,
        scripts: vec![],
        pinned_at: None,
        archived_at: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

/// `ProjectService.bootstrap`: the project at `workspace_root`, created if missing.
fn bootstrap_project(ctx: &ConnCtx, command_id: &str, title: &str, workspace_root: &str) -> Result<(String, bool), Failure> {
    bootstrap_in(&ctx.state.store, command_id, title, workspace_root)
}

/// [`bootstrap_project`] without a connection (the automatic session import).
pub(crate) fn bootstrap_in(store: &crate::store::Store, command_id: &str, title: &str, workspace_root: &str) -> Result<(String, bool), Failure> {
    if let Some(existing) = store.list_projects().map_err(|e| dispatch_error(e.to_string()))?.into_iter().find(|p| p.workspace_root == workspace_root) {
        return Ok((existing.id, false));
    }
    let id = crate::util::new_uuid();
    store
        .create_project(new_project_shell(id.clone(), title.to_owned(), workspace_root.to_owned(), None), Some(command_id))
        .map_err(|e| dispatch_error(e.to_string()))?;
    Ok((id, true))
}

async fn ensure_scratch(ctx: Arc<ConnCtx>, _input: IgnoredAny) -> Result<ProjectEnsureScratchResult, Failure> {
    let root = crate::managed::scratch_root().ok_or_else(|| {
        dispatch_error("Threads without a project are off because Domovoi's home is inside a Git repository. Point --home-dir at a folder outside it, then restart.")
    })?;
    std::fs::create_dir_all(&root).map_err(|e| dispatch_error(format!("Could not create {}: {e}", root.display())))?;
    let root_s = root.to_string_lossy().into_owned();
    let id = crate::util::new_uuid();
    let (project_id, created) = bootstrap_project(&ctx, &format!("scratch-project:{id}"), "No project", &root_s)?;
    if created {
        // Set once at create, so a user's own icon choice is never overwritten.
        let store = &ctx.state.store;
        if let Some(mut p) = store.get_project(&project_id).map_err(|e| dispatch_error(e.to_string()))? {
            p.project_icon = Some(crate::project_icons::scratch_default());
            store.update_project(p, Some(&format!("scratch-project-icon:{id}"))).map_err(|e| dispatch_error(e.to_string()))?;
        }
    }
    Ok(ProjectEnsureScratchResult { project_id: project_id.into() })
}

#[derive(Deserialize)]
struct CreateNewInput {
    name: String,
}

async fn create_new(ctx: Arc<ConnCtx>, input: CreateNewInput) -> Result<ProjectCreateNewResult, Failure> {
    let name = input.name.clone();
    let (workspace_root, commit_error) = blocking(move || crate::managed::scaffold_named_project(&name)).await.map_err(dispatch_error)?;
    let id = crate::util::new_uuid();
    let identity = crate::vcs::repository_identity(&workspace_root).await;
    let shell = new_project_shell(id.clone(), input.name, workspace_root.clone(), identity);
    if let Err(e) = ctx.state.store.create_project(shell, Some(&format!("named-project:{id}"))) {
        let _ = std::fs::remove_dir_all(&workspace_root);
        return Err(dispatch_error(e.to_string()));
    }
    Ok(ProjectCreateNewResult { project_id: id.into(), workspace_root, commit_error })
}

/// Whether the thread has a run queued or under way, or a request waiting for an answer.
fn has_unfinished_work(runs: &[Arc<Run>], requests: &[Arc<RuntimeRequest>]) -> bool {
    let active_run = runs.iter().any(|run| matches!(run.status.as_str(), "queued" | "starting" | "running" | "waiting" | "interrupting" | "pending"));
    let open_request = requests.iter().any(|request| matches!(request.status.as_str(), "pending" | "open"));
    active_run || open_request
}

/// `ScratchThreadPromotion.promote`: a scratch thread's own folder becomes a
/// project, and every scratch thread working in that folder moves into it.
async fn create_from_thread(ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<CreateFromThreadInput>) -> Result<ProjectCreateFromThreadResult, Failure> {
    let store = &ctx.state.store;
    let thread = store
        .app_thread(&input.thread_id)
        .map_err(|_| dispatch_error("Could not read the thread."))?
        .filter(|t| t.deleted_at.is_none())
        .ok_or_else(|| dispatch_error("Thread not found."))?;
    let project = store.get_project(thread.project_id.as_str()).map_err(|_| dispatch_error("Could not read the thread's project."))?;
    let folder = thread.worktree_path.clone().or_else(|| project.as_ref().map(|p| p.workspace_root.clone()));
    let is_scratch = project.as_ref().is_some_and(|p| crate::managed::is_scratch_root(&p.workspace_root));
    let already_promoted = project.as_ref().is_some_and(|p| Some(&p.workspace_root) == folder.as_ref()) && !is_scratch;
    let Some(folder) = folder.filter(|f| crate::managed::is_scratch_thread_folder(f) && (is_scratch || already_promoted)) else {
        return Err(dispatch_error("Only a No project thread with its own folder can become a project."));
    };
    let is_scratch_project =
        |project_id: &ProjectId| store.get_project(project_id.as_str()).ok().flatten().is_some_and(|p| crate::managed::is_scratch_root(&p.workspace_root));
    let mut move_set: Vec<AppThread> = vec![];
    for t in store.app_threads().map_err(|_| dispatch_error("Could not read the threads in this folder."))? {
        let mine = t.id == thread.id && is_scratch;
        if mine || (t.worktree_path.as_deref() == Some(folder.as_str()) && is_scratch_project(&t.project_id)) {
            move_set.push(t);
        }
    }
    for t in &move_set {
        let unreadable = |_| dispatch_error(format!("Could not read \"{}\".", t.title));
        let runs = store.records::<Run>(t.id.as_str()).map_err(unreadable)?;
        let requests = store.records::<RuntimeRequest>(t.id.as_str()).map_err(unreadable)?;
        if has_unfinished_work(&runs, &requests) {
            return Err(dispatch_error(format!("\"{}\" is still working. Finish or stop it first.", t.title)));
        }
    }
    let title = thread.title.clone();
    let (project_id, _) = bootstrap_project(&ctx, &format!("scratch-promotion-project:{}", crate::util::new_uuid()), &title, &folder)?;
    for t in &move_set {
        let adopt = ThreadProjectAdoptFolder {
            command_id: format!("scratch-promotion:{}", crate::util::new_uuid()).into(),
            thread_id: t.id.clone(),
            project_id: project_id.as_str().into(),
        };
        ctx.state
            .orchestrator
            .dispatch(adopt.into())
            .await
            .map_err(|_| dispatch_error(format!("Could not move \"{}\". Try again.", t.title)))?;
    }
    Ok(ProjectCreateFromThreadResult { project_id: project_id.into() })
}

/// The failure of a call that lists a workspace's entries, from why its root or folder was not read.
fn list_failure(cwd: &str, directory_path: Option<Option<String>>) -> impl Fn(EntriesError) -> workspace::Failure {
    move |e| workspace::Failure::ListEntries { cwd: cwd.to_owned(), failure: e.failure, message: e.message, directory_path: directory_path.clone() }
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("projects.listEntries", |_ctx: Arc<ConnCtx>, input: ListEntriesInput| async move {
        blocking(move || {
            let failed = list_failure(&input.cwd, Some(input.directory_path.clone()));
            let root = workspace::normalize_root(&input.cwd).map_err(&failed)?;
            match &input.directory_path {
                Some(dir) => workspace::list_directory(&root, dir).map_err(&failed),
                None => Ok(workspace::list(&root)),
            }
        })
        .await
    })
    .unary("projects.listIgnoredFiles", |_ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<ListIgnoredFilesInput>| async move {
        let cwd = input.cwd;
        blocking(move || {
            let root = workspace::normalize_root(&cwd).map_err(list_failure(&cwd, None))?;
            Ok::<_, workspace::Failure>(workspace::list_ignored_files(&root))
        })
        .await
    })
    .unary("projects.searchEntries", |_ctx: Arc<ConnCtx>, input: SearchEntriesInput| async move {
        blocking(move || {
            let root = workspace::normalize_root(&input.cwd).map_err(|e| workspace::Failure::SearchEntries {
                cwd: input.cwd.clone(),
                failure: e.failure,
                message: e.message,
                query_length: input.query.encode_utf16().count(),
                limit: input.limit,
            })?;
            let image_only = input.image_only.unwrap_or(false);
            // The web picker already sends kind=file with imageOnly. Ordinary file and image searches keep their existing semantics.
            let icon_picker = image_only && input.kind == Some(ProjectEntryKind::File);
            if icon_picker && input.query.trim().is_empty() {
                let (paths, truncated) = crate::project_icons::ranked_paths(&root, false);
                return Ok(domovoi_model::typed::ProjectEntriesResult {
                    truncated: truncated || paths.len() > input.limit,
                    entries: paths.into_iter().take(input.limit).map(|path| domovoi_model::typed::ProjectEntry {
                        path, kind: ProjectEntryKind::File, ignored: None,
                    }).collect(),
                });
            }
            let limit = if icon_picker { workspace::MAX_ENTRIES } else { input.limit };
            let mut result = workspace::search(&root, &input.query, limit, input.kind.as_ref(), image_only);
            if icon_picker {
                result.entries.retain(|entry| crate::project_icons::supported(&entry.path));
                result.truncated |= result.entries.len() > input.limit;
                result.entries.truncate(input.limit);
            }
            Ok::<_, workspace::Failure>(result)
        })
        .await
    })
    .unary("projects.searchContents", |_ctx: Arc<ConnCtx>, input: SearchContentsInput| async move {
        blocking(move || {
            let root = workspace::normalize_root(&input.cwd).map_err(|e| workspace::Failure::SearchContents {
                cwd: input.cwd.clone(),
                failure: e.failure,
                message: e.message,
                query_length: input.query.encode_utf16().count(),
                limit: input.limit,
            })?;
            let q = ContentQuery {
                query: &input.query,
                limit: input.limit,
                case_sensitive: input.case_sensitive,
                whole_word: input.whole_word,
                use_regex: input.use_regex,
            };
            Ok::<_, workspace::Failure>(workspace::search_contents(&root, &q))
        })
        .await
    })
    .unary("projects.writeFile", |ctx: Arc<ConnCtx>, input: WriteFileInput| async move {
        // Only known project roots (and their threads' worktrees) accept writes.
        let known = ctx.state.store.list_projects().unwrap_or_default().iter().any(|p| p.workspace_root == input.cwd)
            || ctx.state.store.app_threads().unwrap_or_default().iter().any(|t| t.worktree_path.as_deref() == Some(input.cwd.as_str()));
        if !known {
            return Err(workspace::Failure::WriteFile {
                cwd: input.cwd,
                relative_path: input.relative_path,
                failure: FileFailure::WorkspacePathOutsideRoot,
                message: "The folder is not a known workspace.".into(),
                operation: None,
            });
        }
        blocking(move || match workspace::write_file(&input.cwd, &input.relative_path, &input.contents) {
            Ok(relative_path) => Ok(WriteFileResult { relative_path }),
            Err(e) => Err(workspace::Failure::WriteFile {
                cwd: input.cwd,
                relative_path: input.relative_path,
                failure: e.failure,
                message: e.message,
                operation: e.operation,
            }),
        })
        .await
    })
    .unary("filesystem.getMetadata", |ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<GetMetadataInput>| async move {
        let home = ctx.state.paths.home.clone();
        let paths = input.paths;
        let entries = blocking(move || paths.iter().map(|p| workspace::metadata(p, &home)).collect()).await;
        Ok::<_, Never>(GetMetadataResult { entries })
    })
    .unary("attachments.createUploadUrl", |ctx: Arc<ConnCtx>, RecordOrNot(request): RecordOrNot<UploadRequest>| async move {
        attachments::issue_upload_url(&ctx.state.paths.userdata, request)
    })
    .unary("attachments.delete", |ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<DeleteAttachmentInput>| async move {
        attachments::delete_pending(&ctx.state.paths.userdata, &input.attachment_id);
        Ok::<_, Never>(())
    })
    .unary("assets.persistChatAttachments", |ctx: Arc<ConnCtx>, RecordOrNot(request): RecordOrNot<PersistRequest>| async move {
        let userdata = ctx.state.paths.userdata.clone();
        blocking(move || attachments::persist_chat_attachments(&userdata, request)).await
    })
    .unary("projects.ensureScratch", ensure_scratch)
    .unary("projects.createNew", create_new)
    .unary("projects.createFromThread", create_from_thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_project_that_was_not_made_is_told_as_a_dispatch_error() {
        assert_eq!(json!(dispatch_error("Thread not found.")), json!({ "_tag": "OrchestrationDispatchCommandError", "message": "Thread not found." }));
        assert_eq!(dispatch_error("Thread not found.").to_string(), "Thread not found.");
    }

    #[test]
    fn inputs_read_what_is_left_out_or_of_another_type_as_empty() {
        let cwd = |sent: serde_json::Value| serde_json::from_value::<RecordOrNot<ListIgnoredFilesInput>>(sent).unwrap().0.cwd;
        assert_eq!((cwd(json!({ "cwd": "/w" })), cwd(json!({})), cwd(json!({ "cwd": 7 })), cwd(json!(null))), ("/w".to_owned(), String::new(), String::new(), String::new()));
        let paths = |sent: serde_json::Value| serde_json::from_value::<RecordOrNot<GetMetadataInput>>(sent).unwrap().0.paths;
        assert_eq!(paths(json!({ "paths": ["/a", 5, null, "/b"] })), ["/a", "/b"]);
        assert!(paths(json!({ "paths": "/a" })).is_empty() && paths(json!({})).is_empty() && paths(json!("x")).is_empty());
        let thread = |sent: serde_json::Value| serde_json::from_value::<RecordOrNot<CreateFromThreadInput>>(sent).unwrap().0.thread_id;
        assert_eq!((thread(json!({ "threadId": "t1" })), thread(json!({ "threadId": null })), thread(json!(["t1"]))), ("t1".to_owned(), String::new(), String::new()));
        let id = |sent: serde_json::Value| serde_json::from_value::<RecordOrNot<DeleteAttachmentInput>>(sent).unwrap().0.attachment_id;
        assert_eq!((id(json!({ "attachmentId": "pending-1" })), id(json!({})), id(json!(5))), ("pending-1".to_owned(), String::new(), String::new()));
    }
}
