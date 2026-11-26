//! Server-side artifact persistence, multi-file workspace bundle serving, and
//! offline React/JSX preview hosting.
//!
//! Durable artifacts are stored in SQLite (`artifacts` + `artifact_versions`)
//! per thread with capped version history. Signed asset routes serve multi-file
//! workspace bundles (`/api/assets/artifact/{token}/{*path}`) and React/JSX
//! previews (`/api/assets/artifact-react/{token}`) safely both locally and over
//! iroh without leaking external network access or bloating native client apps.

use crate::attachments;
use crate::store::Store;
use crate::util::sha256_hex;
use domovoi_model::typed::AssetCreateUrlResult;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const ROUTE_PREFIX_BUNDLE: &str = "/api/assets/artifact";
pub const ROUTE_PREFIX_REACT: &str = "/api/assets/artifact-react";
pub const ROUTE_PREFIX_RUNTIME: &str = "/api/assets/artifact-runtime/v1";

const BUNDLE_KIND: &str = "artifact-bundle-v1";
const REACT_KIND: &str = "artifact-react-v1";
const TTL_MS: i64 = 15 * 60_000;
const MAX_VERSIONS: i64 = 50;
pub const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUNDLE_FILE_BYTES: u64 = 10 * 1024 * 1024;
const REACT_CACHE_BYTES: usize = 16 * 1024 * 1024;
const REACT_CACHE_ENTRIES: usize = 64;

const RUNTIME_JS: &str = include_str!("artifact_runtime.js");
const RUNTIME_CSS: &str = include_str!("artifact_runtime.css");

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    pub id: String,
    pub thread_id: String,
    pub project_id: String,
    pub title: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_url: Option<String>,
    pub latest_version: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactVersionSummary {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_item_id: Option<String>,
    pub byte_length: u64,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDetail {
    pub artifact: ArtifactSummary,
    pub version: u32,
    pub versions: Vec<ArtifactVersionSummary>,
    pub content: String,
}

pub struct UpsertInput<'a> {
    pub id: Option<&'a str>,
    pub thread_id: &'a str,
    pub project_id: &'a str,
    pub title: &'a str,
    pub kind: &'a str,
    pub language: Option<&'a str>,
    pub source_path: Option<&'a str>,
    pub hosted_url: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub turn_item_id: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub content: &'a str,
}

/// Normalizes an artifact kind into the canonical set.
pub fn normalize_kind(kind: &str) -> &'static str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "html" | "htm" | "text/html" => "html",
        "svg" | "image/svg+xml" => "svg",
        "react" | "jsx" | "tsx" | "application/vnd.ant.react" => "react",
        "mermaid" | "mmd" | "application/vnd.ant.mermaid" => "mermaid",
        "markdown" | "md" | "text/markdown" => "markdown",
        "hosted" => "hosted",
        _ => "code",
    }
}

/// Creates a new artifact or appends a new version to an existing one.
pub fn upsert(store: &Store, input: UpsertInput<'_>) -> Result<(ArtifactSummary, u32), String> {
    if input.content.len() > MAX_CONTENT_BYTES {
        return Err(format!("Artifact content exceeds the {} MiB limit.", MAX_CONTENT_BYTES / (1024 * 1024)));
    }
    let now = crate::time_fmt::iso_now();
    let kind = normalize_kind(input.kind);
    let title = input.title.trim();
    let title = if title.is_empty() { "Untitled artifact" } else { title };
    let id = input
        .id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("art_{}", &crate::util::new_uuid().replace('-', "")[..12]));
    let byte_length = input.content.len() as i64;

    store
        .db()
        .with(|conn| {
            let existing: Option<(String, i64, String)> = conn
                .query_row(
                    "SELECT thread_id, latest_version, created_at FROM artifacts WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;

            let (version, created_at) = match existing {
                Some((owner_thread, latest, created_at)) => {
                    if owner_thread != input.thread_id {
                        return Err(rusqlite::Error::InvalidParameterName(
                            "artifact belongs to a different thread".into(),
                        ));
                    }
                    let next = latest + 1;
                    conn.execute(
                        "UPDATE artifacts
                         SET title = ?2, kind = ?3, language = COALESCE(?4, language),
                             source_path = COALESCE(?5, source_path),
                             hosted_url = COALESCE(?6, hosted_url),
                             latest_version = ?7, updated_at = ?8
                         WHERE id = ?1",
                        params![id, title, kind, input.language, input.source_path, input.hosted_url, next, now],
                    )?;
                    (next, created_at)
                }
                None => {
                    conn.execute(
                        "INSERT INTO artifacts (id, thread_id, project_id, title, kind, language, source_path, hosted_url, latest_version, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
                        params![id, input.thread_id, input.project_id, title, kind, input.language, input.source_path, input.hosted_url, now],
                    )?;
                    (1, now.clone())
                }
            };

            conn.execute(
                "INSERT INTO artifact_versions (artifact_id, version, run_id, turn_item_id, summary, content, byte_length, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, version, input.run_id, input.turn_item_id, input.summary, input.content, byte_length, now],
            )?;

            if version > MAX_VERSIONS {
                conn.execute(
                    "DELETE FROM artifact_versions WHERE artifact_id = ?1 AND version <= ?2",
                    params![id, version - MAX_VERSIONS],
                )?;
            }

            Ok((
                ArtifactSummary {
                    id: id.clone(),
                    thread_id: input.thread_id.to_owned(),
                    project_id: input.project_id.to_owned(),
                    title: title.to_owned(),
                    kind: kind.to_owned(),
                    language: input.language.map(str::to_owned),
                    source_path: input.source_path.map(str::to_owned),
                    hosted_url: input.hosted_url.map(str::to_owned),
                    latest_version: version as u32,
                    created_at,
                    updated_at: now.clone(),
                },
                version as u32,
            ))
        })
        .map_err(|e| format!("Could not store artifact: {e}"))
}

pub fn list_for_thread(store: &Store, thread_id: &str) -> Result<Vec<ArtifactSummary>, String> {
    store
        .db()
        .read(|conn| {
            let mut st = conn.prepare_cached(
                "SELECT id, thread_id, project_id, title, kind, language, source_path, hosted_url, latest_version, created_at, updated_at
                 FROM artifacts WHERE thread_id = ?1 ORDER BY updated_at DESC",
            )?;
            let rows = st.query_map(params![thread_id], |row| {
                Ok(ArtifactSummary {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    project_id: row.get(2)?,
                    title: row.get(3)?,
                    kind: row.get(4)?,
                    language: row.get(5)?,
                    source_path: row.get(6)?,
                    hosted_url: row.get(7)?,
                    latest_version: row.get::<_, i64>(8)? as u32,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| format!("Could not list artifacts: {e}"))
}

pub fn read_version(store: &Store, artifact_id: &str, requested_version: Option<u32>) -> Result<Option<ArtifactDetail>, String> {
    store
        .db()
        .read(|conn| {
            let summary: Option<ArtifactSummary> = conn
                .query_row(
                    "SELECT id, thread_id, project_id, title, kind, language, source_path, hosted_url, latest_version, created_at, updated_at
                     FROM artifacts WHERE id = ?1",
                    params![artifact_id],
                    |row| {
                        Ok(ArtifactSummary {
                            id: row.get(0)?,
                            thread_id: row.get(1)?,
                            project_id: row.get(2)?,
                            title: row.get(3)?,
                            kind: row.get(4)?,
                            language: row.get(5)?,
                            source_path: row.get(6)?,
                            hosted_url: row.get(7)?,
                            latest_version: row.get::<_, i64>(8)? as u32,
                            created_at: row.get(9)?,
                            updated_at: row.get(10)?,
                        })
                    },
                )
                .optional()?;
            let Some(artifact) = summary else { return Ok(None) };
            let target_version = requested_version.unwrap_or(artifact.latest_version);
            let content: Option<String> = conn
                .query_row(
                    "SELECT content FROM artifact_versions WHERE artifact_id = ?1 AND version = ?2",
                    params![artifact_id, i64::from(target_version)],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(content) = content else { return Ok(None) };
            let mut st = conn.prepare_cached(
                "SELECT version, summary, turn_item_id, byte_length, created_at
                 FROM artifact_versions WHERE artifact_id = ?1 ORDER BY version ASC",
            )?;
            let versions = st
                .query_map(params![artifact_id], |row| {
                    Ok(ArtifactVersionSummary {
                        version: row.get::<_, i64>(0)? as u32,
                        summary: row.get(1)?,
                        turn_item_id: row.get(2)?,
                        byte_length: row.get::<_, i64>(3)? as u64,
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(ArtifactDetail { artifact, version: target_version, versions, content }))
        })
        .map_err(|e| format!("Could not read artifact: {e}"))
}

// ---- Multi-file workspace bundle serving ----

#[derive(Serialize, Deserialize)]
struct BundleClaims {
    kind: String,
    root: String,
    base_dir: String,
    expires_at: i64,
}

/// Issues a signed URL for a workspace HTML artifact so relative CSS, JS, JSON,
/// and image assets inside the same worktree resolve cleanly over HTTP & iroh.
pub fn issue_bundle(userdata: &Path, cwd: &str, relative_path: &str) -> Result<AssetCreateUrlResult, attachments::Failure> {
    let err = |message: &str| attachments::Failure::AssetUrl { message: message.to_owned() };
    let root = std::fs::canonicalize(cwd).map_err(|_| err("Workspace folder not found."))?;
    let target = root.join(relative_path);
    let resolved = std::fs::canonicalize(&target).map_err(|_| err("Artifact file not found."))?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err(err("Artifact file resolves outside the workspace."));
    }
    let base_dir = resolved.parent().unwrap_or(&root);
    let file_name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| err("Invalid artifact filename."))?;
    let expires_at = crate::time_fmt::now_millis() + TTL_MS;
    let claims = BundleClaims {
        kind: BUNDLE_KIND.into(),
        root: root.to_string_lossy().into_owned(),
        base_dir: base_dir.to_string_lossy().into_owned(),
        expires_at,
    };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign artifact URL."))?;
    let encoded_name = crate::util::encode_uri_component(file_name);
    Ok(AssetCreateUrlResult {
        relative_url: format!("{ROUTE_PREFIX_BUNDLE}/{token}/{encoded_name}"),
        expires_at: expires_at.into(),
        source_path: Some(relative_path.to_owned()),
        image_dimensions: None,
        file_revision: None,
    })
}

fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Reads a single file from a signed workspace artifact bundle, strictly
/// confined to the original canonical workspace root.
pub fn read_bundle(userdata: &Path, token: &str, rel_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let claims: BundleClaims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != BUNDLE_KIND || claims.expires_at <= crate::time_fmt::now_millis() {
        return None;
    }
    if rel_path.is_empty() || rel_path.contains('\0') {
        return None;
    }
    let root = std::fs::canonicalize(&claims.root).ok()?;
    let base_dir = std::fs::canonicalize(&claims.base_dir).ok()?;
    if !base_dir.starts_with(&root) {
        return None;
    }
    let mut joined = PathBuf::from(&base_dir);
    for seg in rel_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                joined.pop();
            }
            other => joined.push(other),
        }
    }
    let resolved = std::fs::canonicalize(&joined).ok()?;
    if !resolved.starts_with(&root) {
        return None;
    }
    let meta = std::fs::metadata(&resolved).ok()?;
    if !meta.is_file() || meta.len() > MAX_BUNDLE_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(&resolved).ok()?;
    Some((mime_for_path(&resolved), bytes))
}

// ---- Offline React / JSX / TSX runtime preview ----

#[derive(Serialize, Deserialize)]
struct ReactClaims {
    kind: String,
    digest: String,
    expires_at: i64,
}

struct CachedReact {
    digest: String,
    html: Vec<u8>,
}

static REACT_CACHE: Mutex<VecDeque<CachedReact>> = Mutex::new(VecDeque::new());

fn react_cache() -> std::sync::MutexGuard<'static, VecDeque<CachedReact>> {
    REACT_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn build_react_document(source: &str, dark: bool) -> Vec<u8> {
    let safe_source = source.replace("</script", "<\\/script");
    let theme_class = if dark { " class=\"dark\"" } else { "" };
    format!(
        r#"<!doctype html>
<html{theme_class}>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'">
<link rel="stylesheet" href="{ROUTE_PREFIX_RUNTIME}/runtime.css">
</head>
<body>
<div id="root"></div>
<script id="domovoi-jsx-source" type="text/domovoi-jsx">{safe_source}</script>
<script src="{ROUTE_PREFIX_RUNTIME}/runtime.js"></script>
</body>
</html>"#
    )
    .into_bytes()
}

pub fn issue_react_preview(userdata: &Path, source: &str, dark: bool) -> Result<AssetCreateUrlResult, attachments::Failure> {
    let err = |message: &str| attachments::Failure::AssetUrl { message: message.to_owned() };
    if source.trim().is_empty() || source.len() > MAX_CONTENT_BYTES {
        return Err(err("This React artifact is empty or too large to preview."));
    }
    let digest = sha256_hex(format!("{}\0{source}", if dark { "dark" } else { "light" }).as_bytes());
    {
        let mut cache = react_cache();
        if let Some(pos) = cache.iter().position(|e| e.digest == digest) {
            if let Some(entry) = cache.remove(pos) {
                cache.push_back(entry);
            }
        } else {
            let html = build_react_document(source, dark);
            while cache.len() >= REACT_CACHE_ENTRIES
                || cache.iter().map(|e| e.html.len()).sum::<usize>() + html.len() > REACT_CACHE_BYTES
            {
                if cache.pop_front().is_none() {
                    break;
                }
            }
            cache.push_back(CachedReact { digest: digest.clone(), html });
        }
    }
    let expires_at = crate::time_fmt::now_millis() + TTL_MS;
    let claims = ReactClaims { kind: REACT_KIND.into(), digest, expires_at };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign React preview URL."))?;
    Ok(AssetCreateUrlResult {
        relative_url: format!("{ROUTE_PREFIX_REACT}/{token}"),
        expires_at: expires_at.into(),
        source_path: None,
        image_dimensions: None,
        file_revision: None,
    })
}

pub fn read_react_preview(userdata: &Path, token: &str) -> Option<Vec<u8>> {
    let claims: ReactClaims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != REACT_KIND || claims.expires_at <= crate::time_fmt::now_millis() {
        return None;
    }
    let mut cache = react_cache();
    let pos = cache.iter().position(|e| e.digest == claims.digest)?;
    let entry = cache.remove(pos)?;
    let html = entry.html.clone();
    cache.push_back(entry);
    Some(html)
}

pub fn read_runtime_asset(file: &str) -> Option<(&'static str, &'static [u8])> {
    match file {
        "runtime.js" => Some(("application/javascript; charset=utf-8", RUNTIME_JS.as_bytes())),
        "runtime.css" => Some(("text/css; charset=utf-8", RUNTIME_CSS.as_bytes())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn artifacts_store_versions_and_enforces_thread_ownership() {
        let store = Store::new(Db::open_in_memory().expect("in-memory db"));
        let (v1, n1) = upsert(
            &store,
            UpsertInput {
                id: Some("dash"),
                thread_id: "t1",
                project_id: "p1",
                title: "Metrics Dashboard",
                kind: "html",
                language: Some("html"),
                source_path: Some("dash.html"),
                hosted_url: None,
                run_id: Some("r1"),
                turn_item_id: Some("i1"),
                summary: Some("Initial"),
                content: "<html><body>v1</body></html>",
            },
        )
        .expect("v1");
        assert_eq!((n1, v1.latest_version), (1, 1));

        let (_, n2) = upsert(
            &store,
            UpsertInput {
                id: Some("dash"),
                thread_id: "t1",
                project_id: "p1",
                title: "Metrics Dashboard v2",
                kind: "html",
                language: Some("html"),
                source_path: None,
                hosted_url: Some("https://claude.ai/artifact/abc"),
                run_id: Some("r2"),
                turn_item_id: Some("i2"),
                summary: Some("Added filter"),
                content: "<html><body>v2</body></html>",
            },
        )
        .expect("v2");
        assert_eq!(n2, 2);

        // Cross-thread overwrite is rejected.
        assert!(upsert(
            &store,
            UpsertInput {
                id: Some("dash"),
                thread_id: "t2",
                project_id: "p1",
                title: "Hijack",
                kind: "html",
                language: None,
                source_path: None,
                hosted_url: None,
                run_id: None,
                turn_item_id: None,
                summary: None,
                content: "x",
            },
        )
        .is_err());

        let listed = list_for_thread(&store, "t1").expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].hosted_url.as_deref(), Some("https://claude.ai/artifact/abc"));
        assert_eq!(listed[0].source_path.as_deref(), Some("dash.html"));

        let d1 = read_version(&store, "dash", Some(1)).expect("read v1").expect("exists");
        assert_eq!(d1.content, "<html><body>v1</body></html>");
        assert_eq!(d1.versions.len(), 2);

        let d_latest = read_version(&store, "dash", None).expect("read latest").expect("exists");
        assert_eq!(d_latest.version, 2);
        assert_eq!(d_latest.content, "<html><body>v2</body></html>");
    }

    #[test]
    fn workspace_bundle_serves_relative_assets_and_blocks_escape() {
        let dir = std::env::temp_dir().join(format!("domovoi-bundle-{}", uuid::Uuid::new_v4()));
        let userdata = dir.join("userdata");
        let work = dir.join("work");
        let sub = work.join("demo");
        std::fs::create_dir_all(&userdata).expect("userdata");
        std::fs::create_dir_all(&sub).expect("sub");
        std::fs::write(sub.join("index.html"), "<html><link rel=stylesheet href=style.css></html>").expect("html");
        std::fs::write(sub.join("style.css"), "body { color: red; }").expect("css");
        std::fs::write(dir.join("secret.txt"), "top-secret").expect("secret");

        let issued = issue_bundle(&userdata, work.to_str().expect("utf8"), "demo/index.html").expect("bundle");
        let rest = issued.relative_url.strip_prefix(&format!("{ROUTE_PREFIX_BUNDLE}/")).expect("prefix");
        let (token, entry) = rest.split_once('/').expect("token/entry");
        assert_eq!(entry, "index.html");

        let (mime, html) = read_bundle(&userdata, token, "index.html").expect("index");
        assert!(mime.starts_with("text/html"));
        assert!(String::from_utf8_lossy(&html).contains("style.css"));

        let (css_mime, css) = read_bundle(&userdata, token, "style.css").expect("css");
        assert!(css_mime.starts_with("text/css"));
        assert_eq!(css, b"body { color: red; }");

        // Escape outside worktree must be rejected.
        assert!(read_bundle(&userdata, token, "../../secret.txt").is_none());
        assert!(read_bundle(&userdata, "badtoken", "index.html").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn react_preview_escapes_script_close_and_serves_runtime() {
        let dir = std::env::temp_dir().join(format!("domovoi-react-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let asset = issue_react_preview(&dir, "export default function App() { return <div></script>Hi</div>; }", true).expect("react");
        let token = asset.relative_url.strip_prefix(&format!("{ROUTE_PREFIX_REACT}/")).expect("prefix");
        let html = String::from_utf8(read_react_preview(&dir, token).expect("html")).expect("utf8");
        assert!(html.contains("class=\"dark\""));
        assert!(html.contains("<\\/script>"));
        assert!(html.contains(&format!("{ROUTE_PREFIX_RUNTIME}/runtime.js")));
        assert!(read_runtime_asset("runtime.js").is_some());
        assert!(read_runtime_asset("runtime.css").is_some());
        assert!(read_runtime_asset("../etc/passwd").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
