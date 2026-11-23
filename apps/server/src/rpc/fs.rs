//! Filesystem RPCs: folder browsing (add-project dialog), project file reads,
//! and asset URLs.

use super::shell::{RecordOrNot, text_or_empty};
use crate::state::ConnCtx;
use crate::workspace::{BrowseFailure, Failure, FileFailure};
use effect_rpc::Router;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use domovoi_model::typed::{
    AssetCreateUrlResult, FilesystemBrowseEntry, FilesystemBrowseResult, LfsObjectState, ProjectFetchLfsObjectResult, ProjectFileLfs,
    ProjectReadFileResult,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowseInput {
    partial_path: String,
    cwd: Option<String>,
}

/// Why the folder a browse named was not listed: it is missing, or reading it failed with `message`.
fn browse_error(input: &BrowseInput, message: &str, missing: bool) -> Failure {
    Failure::Browse {
        partial_path: input.partial_path.clone(),
        message: message.to_owned(),
        cwd: input.cwd.clone(),
        path_missing: missing.then_some(true),
        failure: (!missing).then_some(BrowseFailure::ReadDirectoryFailed),
    }
}

/// Lists directories under the parent of `partialPath` (or `partialPath` itself
/// when it ends in `/`), filtered by the typed prefix, hidden entries excluded.
async fn browse(_ctx: Arc<ConnCtx>, input: BrowseInput) -> Result<FilesystemBrowseResult, Failure> {
    let expanded = crate::util::expand_home(&input.partial_path.replacen('~', "/HOME", 1));
    let raw = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        match &input.cwd {
            Some(cwd) => Path::new(cwd).join(&expanded),
            None => {
                return Err(Failure::Browse {
                    partial_path: input.partial_path,
                    message: "Relative paths need a current project.".into(),
                    cwd: None,
                    path_missing: None,
                    failure: Some(BrowseFailure::CurrentProjectRequired),
                });
            }
        }
    };
    let (dir, prefix) = if input.partial_path.ends_with('/') || raw.as_os_str().is_empty() {
        (raw, String::new())
    } else {
        (raw.parent().map(Path::to_path_buf).unwrap_or(raw.clone()),
         raw.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    };
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(browse_error(&input, "Directory does not exist.", true)),
        Err(e) => return Err(browse_error(&input, &e.to_string(), false)),
    };
    let mut entries: Vec<(String, String)> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.') && name.to_lowercase().starts_with(&prefix.to_lowercase()))
                .then(|| (name, e.path().to_string_lossy().into_owned()))
        })
        .collect();
    entries.sort();
    let parent = dir.to_string_lossy().trim_end_matches('/').to_owned();
    Ok(FilesystemBrowseResult {
        parent_path: if parent.is_empty() { "/".to_owned() } else { parent },
        entries: entries.into_iter().map(|(name, full_path)| FilesystemBrowseEntry { name, full_path }).collect(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadFileInput {
    cwd: String,
    relative_path: String,
    #[serde(default)]
    allow_missing: bool,
    #[serde(default)]
    preview_images: bool,
}

const MAX_READ_BYTES: usize = 1 << 20;

async fn read_file(ctx: Arc<ConnCtx>, input: ReadFileInput) -> Result<ProjectReadFileResult, Failure> {
    let err = |failure: FileFailure, message: String| Failure::ReadFile { cwd: input.cwd.clone(), relative_path: input.relative_path.clone(), failure, message };
    let root = std::fs::canonicalize(&input.cwd).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let target = root.join(&input.relative_path);
    let resolved = match std::fs::canonicalize(&target) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && input.allow_missing => {
            return Ok(ProjectReadFileResult { relative_path: input.relative_path, contents: String::new(), byte_length: 0, truncated: false, image_url: None, exists: Some(false), lfs: None });
        }
        Err(e) => return Err(err(FileFailure::OperationFailed, e.to_string())),
    };
    if !resolved.starts_with(&root) {
        return Err(err(FileFailure::ResolvedPathOutsideRoot, "Path resolves outside the project.".into()));
    }
    if !resolved.is_file() {
        return Err(err(FileFailure::PathNotFile, "Not a file.".into()));
    }
    if input.preview_images
        && let Some(preview) = read_preview(&ctx.state.paths.userdata, &root, &resolved, &input.relative_path)
            .await
            .map_err(|message| err(FileFailure::OperationFailed, message))?
    {
        return Ok(preview);
    }
    let bytes = std::fs::read(&resolved).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let truncated = bytes.len() > MAX_READ_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_READ_BYTES)];
    let contents = String::from_utf8(slice.to_vec()).map_err(|_| err(FileFailure::BinaryFile, "Binary file.".into()))?;
    Ok(ProjectReadFileResult { relative_path: input.relative_path, contents, byte_length: bytes.len() as u64, truncated, image_url: None, exists: Some(true), lfs: None })
}

/// Text as `projects.readFile` gives it (at most 1 MiB); empty for a binary file.
fn text_of(bytes: &[u8]) -> (String, bool) {
    let slice = &bytes[..bytes.len().min(MAX_READ_BYTES)];
    match std::str::from_utf8(slice) {
        Ok(text) => (text.to_owned(), bytes.len() > MAX_READ_BYTES),
        Err(_) => (String::new(), false),
    }
}

/// A `previewImages` read (Domovoi's clients): an image's link, and for a
/// file kept in Git LFS its object (from the repository's cache) and state.
/// `None`: an ordinary file, read as t3code reads it.
async fn read_preview(userdata: &Path, root: &Path, resolved: &Path, relative_path: &str) -> Result<Option<ProjectReadFileResult>, String> {
    let directory = resolved.parent().unwrap_or(root);
    let name = resolved.file_name().and_then(|n| n.to_str()).unwrap_or(relative_path);
    let head = crate::lfs::head(resolved).await.map_err(|e| e.to_string())?;
    let pointer = crate::lfs::parse_pointer(&head);
    let reply = |contents: String, byte_length: u64, truncated: bool, image_url: Option<String>, lfs: Option<ProjectFileLfs>| ProjectReadFileResult {
        relative_path: relative_path.to_owned(),
        contents,
        byte_length,
        truncated,
        image_url,
        exists: Some(true),
        lfs,
    };
    // A pointer whose object isn't shown: its text, and why.
    let unresolved = |pointer: &crate::lfs::Pointer, state: LfsObjectState| {
        let lfs = ProjectFileLfs { oid: pointer.oid.clone(), size: pointer.size, state };
        reply(String::from_utf8_lossy(&head).into_owned(), pointer.size, false, None, Some(lfs))
    };
    match crate::file_images::issue(userdata, root, resolved, relative_path).await {
        Ok(Some(image)) => {
            let lfs = if pointer.is_some() || crate::lfs::tracked(directory, name).await {
                Some(ProjectFileLfs { oid: image.digest, size: image.byte_length, state: LfsObjectState::Cached })
            } else {
                None
            };
            return Ok(Some(reply(String::new(), image.byte_length, false, Some(image.url), lfs)));
        }
        Ok(None) => {}
        Err(crate::file_images::ImageError::Lfs(state)) => {
            let pointer = pointer.ok_or("The Git LFS pointer changed while it was read.")?;
            return Ok(Some(unresolved(&pointer, state)));
        }
        Err(crate::file_images::ImageError::Message(message)) => return Err(message),
    }
    if let Some(pointer) = pointer {
        return Ok(Some(match crate::lfs::read_object(directory, &pointer, crate::file_images::MAX_BYTES).await {
            Ok(object) => {
                let (contents, truncated) = text_of(&object);
                let lfs = ProjectFileLfs { oid: pointer.oid, size: pointer.size, state: LfsObjectState::Cached };
                reply(contents, pointer.size, truncated, None, Some(lfs))
            }
            Err(state) => unresolved(&pointer, state),
        }));
    }
    // Kept in LFS and checked out: the file is the object.
    if crate::lfs::tracked(directory, name).await {
        let bytes = tokio::fs::read(resolved).await.map_err(|e| e.to_string())?;
        let (contents, truncated) = text_of(&bytes);
        let lfs = ProjectFileLfs { oid: crate::util::sha256_hex(&bytes), size: bytes.len() as u64, state: LfsObjectState::Cached };
        return Ok(Some(reply(contents, bytes.len() as u64, truncated, None, Some(lfs))));
    }
    Ok(None)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchLfsObjectInput {
    cwd: String,
    relative_path: String,
}

/// Domovoi's own `projects.fetchLfsObject`: downloads a Git LFS file's
/// object into the repository's cache (the working tree is left as it is),
/// so its preview shows the object.
async fn fetch_lfs_object(_ctx: Arc<ConnCtx>, input: FetchLfsObjectInput) -> Result<ProjectFetchLfsObjectResult, Failure> {
    let err = |failure: FileFailure, message: String| Failure::ReadFile { cwd: input.cwd.clone(), relative_path: input.relative_path.clone(), failure, message };
    let root = std::fs::canonicalize(&input.cwd).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let resolved = std::fs::canonicalize(root.join(&input.relative_path)).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    if !resolved.starts_with(&root) {
        return Err(err(FileFailure::ResolvedPathOutsideRoot, "Path resolves outside the project.".into()));
    }
    if !resolved.is_file() {
        return Err(err(FileFailure::PathNotFile, "Not a file.".into()));
    }
    let directory = resolved.parent().unwrap_or(&root);
    let name = resolved.file_name().and_then(|n| n.to_str()).unwrap_or(&input.relative_path);
    let head = crate::lfs::head(&resolved).await.map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let state = match crate::lfs::parse_pointer(&head) {
        Some(pointer) => crate::lfs::fetch(directory, name, &pointer).await.map_err(|message| err(FileFailure::OperationFailed, message))?,
        // Checked out already: the file is the object.
        None if crate::lfs::tracked(directory, name).await => LfsObjectState::Cached,
        None => return Err(err(FileFailure::OperationFailed, "This file isn't kept in Git LFS.".into())),
    };
    Ok(ProjectFetchLfsObjectResult { relative_path: input.relative_path, state })
}

/// `AssetCreateUrlInput` for attachments and project image assets.
#[derive(Deserialize, Default)]
struct CreateAssetUrlInput {
    #[serde(default)]
    resource: RecordOrNot<AssetResource>,
}

/// The fields needed for attachments, saved project icons, picker thumbnails,
/// and Mermaid diagrams (`mermaid-diagram`, Domovoi's own: `source` and a
/// `theme` of `dark` or `light`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AssetResource {
    #[serde(default, rename = "_tag", deserialize_with = "text_or_empty")]
    tag: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    attachment_id: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    cwd: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    path: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    source: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    theme: String,
    /// `media-file` and `tool-output-image`: the thread the image is shown in.
    #[serde(default, deserialize_with = "text_or_empty")]
    thread_id: String,
    /// `tool-output-image`: the tool call, and which of its result's images.
    #[serde(default, deserialize_with = "text_or_empty")]
    item_id: String,
    #[serde(default)]
    index: usize,
}

/// An unresolved icon uses the existing missing asset response.
async fn create_asset_url(ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<CreateAssetUrlInput>) -> Result<AssetCreateUrlResult, crate::attachments::Failure> {
    let RecordOrNot(resource) = input.resource;
    if resource.tag == "attachment" {
        return crate::attachments::issue_asset_url(&ctx.state.paths.userdata, &resource.attachment_id);
    }
    // An image an agent looked at, anywhere on this machine.
    if resource.tag == "media-file"
        && let Some((relative_url, expires_at)) = crate::file_images::issue_media(&ctx.state.paths.userdata, &resource.path)
            .await
            .map_err(|message| crate::attachments::Failure::AssetUrl { message })?
    {
        return Ok(AssetCreateUrlResult { relative_url, expires_at: expires_at.into(), source_path: Some(resource.path), image_dimensions: None, file_revision: None });
    }
    if resource.tag == "tool-output-image" {
        return crate::tool_images::issue(&ctx.state.paths.userdata, &ctx.state.store, &resource.thread_id, &resource.item_id, resource.index);
    }
    if resource.tag == "mermaid-diagram" {
        return crate::diagrams::issue(&ctx.state.paths.userdata, &resource.source, resource.theme == "dark").await;
    }
    if resource.tag == "mermaid-diagram-svg" {
        return crate::diagrams::issue_svg(&ctx.state.paths.userdata, &resource.source, resource.theme == "dark").await;
    }
    if resource.tag == "artifact-bundle" {
        return crate::artifacts::issue_bundle(&ctx.state.paths.userdata, &resource.cwd, &resource.path);
    }
    if resource.tag == "artifact-react" {
        return crate::artifacts::issue_react_preview(&ctx.state.paths.userdata, &resource.source, resource.theme == "dark");
    }
    if matches!(resource.tag.as_str(), "project-favicon" | "draft-workspace-file")
        && let Ok(projects) = ctx.state.store.list_projects()
        && let Some(project) = projects.into_iter().find(|project| project.workspace_root == resource.cwd)
    {
        let thumbnail = resource.tag == "draft-workspace-file";
        let selected = if thumbnail { Some(resource.path.as_str()) } else { project.favicon_path.as_deref() };
        let automatic = !thumbnail && !crate::managed::is_scratch_root(&project.workspace_root);
        if let Some(asset) = crate::project_icons::asset(&ctx.state.paths.userdata, &project.workspace_root, selected, automatic).await {
            return Ok(asset);
        }
    }
    let expires_at = domovoi_contracts::now_millis() + crate::file_images::TTL_MS;
    Ok(AssetCreateUrlResult {
        relative_url: "/api/assets/none/project-favicon-missing".into(),
        expires_at: expires_at.into(),
        source_path: None,
        image_dimensions: None,
        file_revision: None,
    })
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("filesystem.browse", browse)
        .unary("projects.readFile", read_file)
        .unary("projects.fetchLfsObject", fetch_lfs_object)
        .unary("assets.createUrl", create_asset_url)
}
