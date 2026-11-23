//! Read-only image previews for project files and locally cached Git LFS objects.
//!
//! PNG, JPEG, GIF and WebP are served as they are; SVG, ICO, BMP and TIFF
//! are drawn as PNG here, so every client can show them.

use crate::{attachments, lfs, util::sha256_hex, workspace::sniff_mime};
use domovoi_model::typed::LfsObjectState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::fs::{self, File};
use tokio::io::AsyncReadExt;

pub const ROUTE_PREFIX: &str = "/api/assets/project-image";
const KIND: &str = "project-image-read-v1";
pub const TTL_MS: i64 = 5 * 60_000;
/// The most a preview reads of a file (or of a Git LFS object).
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
const MAX_PIXELS: usize = 32_000_000;
const TOO_LARGE: &str = "This image exceeds the 20 MiB preview limit.";
const BAD_IMAGE: &str = "This file could not be read as a supported image.";

/// Why an image has no preview.
#[derive(Debug, PartialEq)]
pub enum ImageError {
    /// A Git LFS pointer whose object can't be shown (not downloaded, too
    /// large, not matching): the reader says so beside the pointer.
    Lfs(LfsObjectState),
    Message(String),
}

impl From<&str> for ImageError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}

impl ImageError {
    fn message(self) -> String {
        match self {
            Self::Message(message) => message,
            Self::Lfs(_) => "This Git LFS image can't be shown.".into(),
        }
    }
}

/// A project image's preview link.
pub struct Issued {
    pub url: String,
    pub byte_length: u64,
    /// SHA-256 of the image: its Git LFS oid, when it is kept there.
    pub digest: String,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    kind: String,
    root: PathBuf,
    relative_path: String,
    digest: String,
    expires_at: i64,
    #[serde(default)]
    icon: bool,
}

pub async fn issue(
    userdata: &Path,
    root: &Path,
    resolved: &Path,
    relative_path: &str,
) -> Result<Option<Issued>, ImageError> {
    let Some(image) = load(resolved, Path::new(relative_path), false).await? else {
        return Ok(None);
    };
    let claims = Claims {
        kind: KIND.into(),
        root: root.to_owned(),
        relative_path: relative_path.into(),
        digest: image.digest.clone(),
        expires_at: domovoi_contracts::now_millis() + TTL_MS,
        icon: false,
    };
    let token = attachments::sign_asset_claims(userdata, &claims)
        .map_err(|_| "Could not open an image preview.")?;
    Ok(Some(Issued {
        url: format!("{ROUTE_PREFIX}/{token}"),
        byte_length: image.bytes.len() as u64,
        digest: image.digest,
    }))
}

/// How long a link to an image an agent looked at keeps working: as long
/// as a chat attachment's (the file's digest is in it, so a changed file
/// stops matching).
const MEDIA_TTL_MS: i64 = 7 * 24 * 60 * 60_000;

/// A URL for an image file anywhere on this machine that an agent looked at
/// (`media-file`): an absolute local path to a supported image, at most 20 MiB.
/// `None` when it is gone or is no image.
pub async fn issue_media(userdata: &Path, path: &str) -> Result<Option<(String, i64)>, String> {
    let given = Path::new(path);
    // A network path (`//host/share`) is never read on a link's behalf.
    if !given.is_absolute() || path.starts_with("//") || path.starts_with("\\\\") || image_extension(given).is_none() {
        return Ok(None);
    }
    let Ok(resolved) = fs::canonicalize(given).await else { return Ok(None) };
    let (Some(root), Some(name)) = (resolved.parent(), resolved.file_name().and_then(|n| n.to_str())) else { return Ok(None) };
    if !fs::metadata(&resolved).await.is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let Some(image) = load(&resolved, Path::new(name), false).await.map_err(ImageError::message)? else { return Ok(None) };
    let expires_at = domovoi_contracts::now_millis() + MEDIA_TTL_MS;
    let claims = Claims { kind: KIND.into(), root: root.to_owned(), relative_path: name.into(), digest: image.digest, expires_at, icon: false };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| "Could not open an image preview.")?;
    Ok(Some((format!("{ROUTE_PREFIX}/{token}"), expires_at)))
}

pub async fn issue_icon(
    userdata: &Path, root: &Path, resolved: &Path, relative_path: &str,
) -> Option<(String, String, i64)> {
    let image = load(resolved, Path::new(relative_path), true).await.ok()??;
    let digest = image.digest.clone();
    // Verify that the source can be rendered before returning it as an automatic choice.
    tokio::task::spawn_blocking(move || raster_png(&image, Some(ICON_SIDE))).await.ok()??;
    let expires_at = domovoi_contracts::now_millis() + TTL_MS;
    let claims = Claims {
        kind: KIND.into(),
        root: root.to_owned(),
        relative_path: relative_path.into(),
        digest: digest.clone(),
        expires_at,
        icon: true,
    };
    let token = attachments::sign_asset_claims(userdata, &claims).ok()?;
    Some((format!("{ROUTE_PREFIX}/{token}"), digest, expires_at))
}

const ICON_SIDE: u32 = 128;

/// `source` as PNG, fitted into `side` × `side` when given.
fn raster_png(source: &Image, side: Option<u32>) -> Option<Vec<u8>> {
    if source.mime == "image/svg+xml" {
        return svg_to_png(&source.bytes, side.map_or(2048.0, |s| s as f32));
    }
    let format = match source.mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        "image/x-icon" => image::ImageFormat::Ico,
        "image/bmp" => image::ImageFormat::Bmp,
        "image/tiff" => image::ImageFormat::Tiff,
        _ => return None,
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&source.bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some((MAX_PIXELS * 4) as u64);
    reader.limits(limits);
    let mut decoded = reader.decode().ok()?;
    if let Some(side) = side {
        decoded = decoded.thumbnail(side, side);
    }
    let mut png = std::io::Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// Formats not every client draws: served as PNG.
fn drawn_as_png(mime: &str) -> bool {
    matches!(mime, "image/svg+xml" | "image/x-icon" | "image/bmp" | "image/tiff")
}

pub async fn read(userdata: &Path, token: &str) -> Option<(&'static str, Vec<u8>)> {
    let claims: Claims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != KIND || claims.expires_at <= domovoi_contracts::now_millis() {
        return None;
    }
    let path = Path::new(&claims.relative_path);
    let resolved = fs::canonicalize(claims.root.join(path)).await.ok()?;
    if !resolved.starts_with(&claims.root) || !fs::metadata(&resolved).await.ok()?.is_file() {
        return None;
    }
    let image = load(&resolved, path, claims.icon).await.ok()??;
    if image.digest != claims.digest {
        return None;
    }
    if claims.icon {
        Some(("image/png", tokio::task::spawn_blocking(move || raster_png(&image, Some(ICON_SIDE))).await.ok()??))
    } else if drawn_as_png(image.mime) {
        Some(("image/png", tokio::task::spawn_blocking(move || raster_png(&image, None)).await.ok()??))
    } else {
        Some((image.mime, image.bytes))
    }
}

struct Image {
    mime: &'static str,
    bytes: Vec<u8>,
    digest: String,
}

/// The image type a file's extension names, of those previews show.
pub fn image_extension(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "ico" => Some("image/x-icon"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

/// The image `head` starts, by its signature. ICO, BMP and TIFF signatures
/// are short, so they only count with their file extension.
fn image_mime(head: &[u8], extension: Option<&'static str>) -> Option<&'static str> {
    sniff_mime(head).filter(|mime| mime.starts_with("image/")).or_else(|| {
        let expected = extension?;
        let matches = match expected {
            "image/svg+xml" => true,
            "image/x-icon" => head.starts_with(&[0, 0, 1, 0]),
            "image/bmp" => head.starts_with(b"BM"),
            "image/tiff" => head.starts_with(b"II*\0") || head.starts_with(b"MM\0*"),
            _ => false,
        };
        matches.then_some(expected)
    })
}

async fn load(resolved: &Path, path: &Path, icon: bool) -> Result<Option<Image>, ImageError> {
    let extension = image_extension(path);
    let detect = |bytes: &[u8]| {
        if icon && bytes.starts_with(&[0, 0, 1, 0]) {
            Some("image/x-icon")
        } else {
            image_mime(bytes, extension)
        }
    };
    let mut file = File::open(resolved).await.map_err(|e| ImageError::Message(e.to_string()))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(lfs::POINTER_LIMIT as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| ImageError::Message(e.to_string()))?;

    let pointer = extension.and_then(|_| lfs::parse_pointer(&bytes));
    let (mime, digest) = if let Some(pointer) = pointer {
        let directory = resolved.parent().ok_or(ImageError::Lfs(LfsObjectState::Missing))?;
        bytes = lfs::read_object(directory, &pointer, MAX_BYTES).await.map_err(ImageError::Lfs)?;
        (detect(&bytes).ok_or(BAD_IMAGE)?, pointer.oid)
    } else {
        let mime = match detect(&bytes) {
            Some(mime) => mime,
            None if extension.is_some() => return Err(BAD_IMAGE.into()),
            None => return Ok(None),
        };
        if file.metadata().await.map_err(|e| ImageError::Message(e.to_string()))?.len() > MAX_BYTES {
            return Err(TOO_LARGE.into());
        }
        file.take(MAX_BYTES + 1 - bytes.len() as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ImageError::Message(e.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(TOO_LARGE.into());
        }
        (mime, sha256_hex(&bytes))
    };
    if mime != "image/svg+xml" && mime != "image/x-icon" {
        let size = imagesize::blob_size(&bytes).map_err(|_| BAD_IMAGE)?;
        if size.width == 0 || size.height == 0 {
            return Err(BAD_IMAGE.into());
        }
        if size.width.saturating_mul(size.height) > MAX_PIXELS {
            return Err("This image exceeds the 32 megapixel preview limit.".into());
        }
    }
    Ok(Some(Image {
        mime,
        bytes,
        digest,
    }))
}

fn svg_to_png(bytes: &[u8], max_side: f32) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let xml = roxmltree::Document::parse_with_options(
        std::str::from_utf8(bytes).ok()?,
        roxmltree::ParsingOptions {
            nodes_limit: 50_000,
            ..Default::default()
        },
    )
    .ok()?;
    let embedded_budget = parking_lot::Mutex::new(MAX_PIXELS);
    let decode_embedded = usvg::ImageHrefResolver::default_data_resolver();
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, data, options| {
                let mime = image_mime(&data, None)?;
                let size = imagesize::blob_size(&data).ok()?;
                let pixels = size.width.checked_mul(size.height)?;
                let mut remaining = embedded_budget.lock();
                *remaining = remaining.checked_sub(pixels)?;
                decode_embedded(mime, data, options)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: FONTS
            .get_or_init(|| {
                let mut db = usvg::fontdb::Database::new();
                db.load_system_fonts();
                Arc::new(db)
            })
            .clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_xmltree(&xml, &options).ok()?;
    let (width, height) = (tree.size().width(), tree.size().height());
    let scale = (max_side / width.max(height)).min(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(
        (width * scale).ceil() as u32,
        (height * scale).ceil() as u32,
    )?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_image_an_agent_viewed_gets_a_link_anywhere_but_nothing_else_does() {
        let dir = std::env::temp_dir().join(format!("domovoi-media-{}", crate::util::new_uuid()));
        std::fs::create_dir(&dir).unwrap();
        let shot = dir.join("shot.svg");
        std::fs::write(&shot, r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#).unwrap();
        std::fs::write(dir.join("notes.txt"), "text").unwrap();
        let (url, _) = issue_media(&dir, shot.to_str().unwrap()).await.unwrap().expect("a link");
        let token = url.strip_prefix(&format!("{ROUTE_PREFIX}/")).unwrap();
        assert_eq!(read(&dir, token).await.map(|(mime, _)| mime), Some("image/png"), "drawn as PNG");
        assert!(issue_media(&dir, dir.join("notes.txt").to_str().unwrap()).await.unwrap().is_none(), "not an image");
        assert!(issue_media(&dir, "shot.svg").await.unwrap().is_none(), "not an absolute path");
        assert!(issue_media(&dir, dir.join("gone.png").to_str().unwrap()).await.unwrap().is_none());
        // The file changed: the link no longer matches it.
        std::fs::write(&shot, r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"/>"#).unwrap();
        assert!(read(&dir, token).await.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn expired_image_claim_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("domovoi-image-link-{}", crate::util::new_uuid()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("picture.svg");
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#,
        )
        .unwrap();
        let digest = sha256_hex(&std::fs::read(&path).unwrap());
        let token = |expires_at| {
            attachments::sign_asset_claims(
                &dir,
                &Claims {
                    kind: KIND.into(),
                    root: dir.canonicalize().unwrap(),
                    relative_path: "picture.svg".into(),
                    digest: digest.clone(),
                    expires_at,
                    icon: false,
                },
            )
            .unwrap()
        };
        assert!(
            read(&dir, &token(domovoi_contracts::now_millis() + TTL_MS))
                .await
                .is_some()
        );
        assert!(
            read(&dir, &token(domovoi_contracts::now_millis() - 1))
                .await
                .is_none()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
