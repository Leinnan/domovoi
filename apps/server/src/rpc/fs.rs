//! Filesystem RPCs: folder browsing (add-project dialog), project file reads,
//! and asset URLs.

use super::shell::{RecordOrNot, text_or_empty};
use crate::state::ConnCtx;
use crate::workspace::{BrowseFailure, Failure, FileFailure};
use effect_rpc::Router;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use domovoi_model::typed::{AssetCreateUrlResult, FilesystemBrowseEntry, FilesystemBrowseResult, ProjectReadFileResult};

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
}

const MAX_READ_BYTES: usize = 1 << 20;

async fn read_file(_ctx: Arc<ConnCtx>, input: ReadFileInput) -> Result<ProjectReadFileResult, Failure> {
    let err = |failure: FileFailure, message: String| Failure::ReadFile { cwd: input.cwd.clone(), relative_path: input.relative_path.clone(), failure, message };
    let root = std::fs::canonicalize(&input.cwd).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let target = root.join(&input.relative_path);
    let resolved = match std::fs::canonicalize(&target) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && input.allow_missing => {
            return Ok(ProjectReadFileResult { relative_path: input.relative_path, contents: String::new(), byte_length: 0, truncated: false, exists: Some(false) });
        }
        Err(e) => return Err(err(FileFailure::OperationFailed, e.to_string())),
    };
    if !resolved.starts_with(&root) {
        return Err(err(FileFailure::ResolvedPathOutsideRoot, "Path resolves outside the project.".into()));
    }
    if !resolved.is_file() {
        return Err(err(FileFailure::PathNotFile, "Not a file.".into()));
    }
    let bytes = std::fs::read(&resolved).map_err(|e| err(FileFailure::OperationFailed, e.to_string()))?;
    let truncated = bytes.len() > MAX_READ_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_READ_BYTES)];
    let contents = String::from_utf8(slice.to_vec()).map_err(|_| err(FileFailure::BinaryFile, "Binary file.".into()))?;
    Ok(ProjectReadFileResult { relative_path: input.relative_path, contents, byte_length: bytes.len() as u64, truncated, exists: Some(true) })
}

/// `AssetCreateUrlInput`, of which only an attachment is served: a resource
/// with any other tag, or none, is answered as a project favicon is.
#[derive(Deserialize, Default)]
struct CreateAssetUrlInput {
    #[serde(default)]
    resource: RecordOrNot<AssetResource>,
}

/// The part of an `AssetResource` that is read: its tag and, for an
/// attachment, its id (one left out, or no text, names no attachment).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AssetResource {
    #[serde(default, rename = "_tag", deserialize_with = "text_or_empty")]
    tag: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    attachment_id: String,
}

/// Favicons are not discovered yet: hand out a URL whose route answers 404,
/// which the client renders as "no favicon".
async fn create_asset_url(ctx: Arc<ConnCtx>, RecordOrNot(input): RecordOrNot<CreateAssetUrlInput>) -> Result<AssetCreateUrlResult, crate::attachments::Failure> {
    let RecordOrNot(resource) = input.resource;
    if resource.tag == "attachment" {
        return crate::attachments::issue_asset_url(&ctx.state.paths.userdata, &resource.attachment_id);
    }
    let expires_at = domovoi_contracts::now_millis() + 60 * 60 * 1000;
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
        .unary("assets.createUrl", create_asset_url)
}
