//! Filename discovery and signed previews for a project's icon.

use crate::{file_images, workspace};
use domovoi_model::typed::{
    AssetCreateUrlResult, LucideProjectIcon, ProjectIcon, ProjectIconColor,
};
use std::path::{Component, Path};

pub fn scratch_default() -> ProjectIcon {
    ProjectIcon::Lucide(LucideProjectIcon {
        name: "message-square-dashed".into(),
        color: ProjectIconColor::Gray,
        monogram_text: None,
        monogram: None,
    })
}

pub fn supported(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico"
            )
        })
}

fn rank(relative: &str, project_name: &str) -> u8 {
    let path = Path::new(relative);
    let parent = path
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let stem_key = stem.replace(['-', '_', ' '], "");
    let conventional = matches!(
        parent.as_str(),
        "" | "public" | "app" | "src" | "src/app" | "static"
    );
    let favicon = stem_key == "favicon";
    let numbered_favicon = stem
        .strip_prefix("favicon-")
        .or_else(|| stem.strip_prefix("favicon_"))
        .is_some_and(|size| {
            !size.is_empty() && size.bytes().all(|c| c.is_ascii_digit() || c == b'x')
        });
    let app_icon = matches!(stem_key.as_str(), "appicon" | "appletouchicon" | "icon");
    if conventional && favicon {
        0
    } else if conventional && numbered_favicon {
        1
    } else if conventional && app_icon {
        2
    } else if favicon || numbered_favicon || app_icon {
        3
    } else if stem.contains("logo")
        || stem.contains("wordmark")
        || (!project_name.is_empty() && stem.contains(project_name))
    {
        4
    } else {
        5
    }
}

pub fn ranked_paths(root: &Path, automatic: bool) -> (Vec<String>, bool) {
    let index = workspace::index(root);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut paths: Vec<_> = index
        .entries
        .iter()
        .filter(|entry| !entry.dir && supported(&entry.path))
        .filter_map(|entry| {
            let priority = rank(&entry.path, &name);
            (!automatic || priority <= 2).then(|| {
                (
                    priority,
                    entry.path.matches('/').count(),
                    entry.path.to_ascii_lowercase(),
                    &entry.path,
                )
            })
        })
        .collect();
    paths.sort_unstable();
    (
        paths
            .into_iter()
            .map(|(_, _, _, path)| path.clone())
            .collect(),
        index.truncated,
    )
}

async fn checked_asset(
    userdata: &Path,
    root: &Path,
    relative: &str,
) -> Option<AssetCreateUrlResult> {
    if !supported(relative)
        || relative.contains('\\')
        || relative.len() > 1024
        || !Path::new(relative)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let target = tokio::fs::canonicalize(root.join(relative)).await.ok()?;
    if !target.starts_with(root) || !tokio::fs::metadata(&target).await.ok()?.is_file() {
        return None;
    }
    let (relative_url, revision, expires_at) =
        file_images::issue_icon(userdata, root, &target, relative).await?;
    Some(AssetCreateUrlResult {
        relative_url,
        expires_at: expires_at.into(),
        source_path: Some(relative.into()),
        image_dimensions: None,
        file_revision: Some(revision),
    })
}

pub async fn asset(
    userdata: &Path,
    cwd: &str,
    selected: Option<&str>,
    automatic: bool,
) -> Option<AssetCreateUrlResult> {
    let root = tokio::fs::canonicalize(cwd).await.ok()?;
    if let Some(path) = selected {
        return checked_asset(userdata, &root, path).await;
    }
    if !automatic {
        return None;
    }
    let (paths, _) = tokio::task::spawn_blocking({
        let root = root.clone();
        move || ranked_paths(&root, true)
    })
    .await
    .ok()?;
    for path in paths.into_iter().take(32) {
        if let Some(asset) = checked_asset(userdata, &root, &path).await {
            return Some(asset);
        }
    }
    None
}
