//! Mermaid diagrams drawn for clients: `assets.createUrl` with a
//! `mermaid-diagram` resource renders the source to a PNG and hands out a
//! short-lived signed URL for it (`ROUTE_PREFIX`). The renderer lives here
//! rather than in the client core so the apps don't each carry it.

use crate::{attachments, util::sha256_hex};
use domovoi_model::typed::AssetCreateUrlResult;
use merman::svg::export::{NativeFontContext, NativeFontLimits, RasterFitBox, RasterOptions, bundled_font_data};
use merman::{MermaidEngine, OperationControl, PngRequest, RenderOutput, RenderRequest, Renderer, SourcePresentationConfig, SvgRequest};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const ROUTE_PREFIX: &str = "/api/assets/diagram";
pub const ROUTE_PREFIX_SVG: &str = "/api/assets/diagram-svg";
const KIND: &str = "mermaid-diagram-v1";
const KIND_SVG: &str = "mermaid-diagram-svg-v1";
const TTL_MS: i64 = 5 * 60_000;
/// Longer sources are shown as text.
pub const SOURCE_LIMIT: usize = 50_000;
/// The rendered PNGs kept for their URLs; the oldest go first.
const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 128;

#[derive(Serialize, Deserialize)]
struct Claims {
    kind: String,
    digest: String,
    expires_at: i64,
}

struct Cached {
    digest: String,
    png: Vec<u8>,
}

static CACHE: Mutex<VecDeque<Cached>> = Mutex::new(VecDeque::new());

fn cache() -> std::sync::MutexGuard<'static, VecDeque<Cached>> {
    CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cached(digest: &str) -> Option<Vec<u8>> {
    let mut cache = cache();
    let index = cache.iter().position(|entry| entry.digest == digest)?;
    let entry = cache.remove(index)?;
    let png = entry.png.clone();
    cache.push_back(entry);
    Some(png)
}

fn remember(digest: String, png: Vec<u8>) {
    if png.len() > CACHE_BYTES {
        return;
    }
    let mut cache = cache();
    cache.retain(|entry| entry.digest != digest);
    while cache.len() >= CACHE_ENTRIES || cache.iter().map(|entry| entry.png.len()).sum::<usize>() + png.len() > CACHE_BYTES {
        cache.pop_front();
    }
    cache.push_back(Cached { digest, png });
}

/// A URL for `source` drawn in the `dark` or light theme, or why there is none.
pub async fn issue(userdata: &Path, source: &str, dark: bool) -> Result<AssetCreateUrlResult, attachments::Failure> {
    let err = |message: &str| attachments::Failure::AssetUrl { message: message.to_owned() };
    if source.trim().is_empty() || source.len() > SOURCE_LIMIT {
        return Err(err("This diagram is empty or too long to draw."));
    }
    let digest = sha256_hex(format!("{}\0{source}", if dark { "dark" } else { "light" }).as_bytes());
    if cached(&digest).is_none() {
        let owned = source.to_owned();
        let png = tokio::task::spawn_blocking(move || std::panic::catch_unwind(|| render(&owned, dark)).ok().flatten())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| err("This diagram couldn’t be rendered."))?;
        remember(digest.clone(), png);
    }
    let expires_at = crate::time_fmt::now_millis() + TTL_MS;
    let claims = Claims { kind: KIND.into(), digest, expires_at };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign the URL."))?;
    Ok(AssetCreateUrlResult {
        relative_url: format!("{ROUTE_PREFIX}/{token}"),
        expires_at: expires_at.into(),
        source_path: None,
        image_dimensions: None,
        file_revision: None,
    })
}

/// The PNG behind a URL `issue` handed out, while it is valid and kept.
pub fn read(userdata: &Path, token: &str) -> Option<Vec<u8>> {
    let claims: Claims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != KIND || claims.expires_at <= crate::time_fmt::now_millis() {
        return None;
    }
    cached(&claims.digest)
}

/// A signed URL for `source` drawn as crisp vector SVG in `dark` or light theme.
pub async fn issue_svg(userdata: &Path, source: &str, dark: bool) -> Result<AssetCreateUrlResult, attachments::Failure> {
    let err = |message: &str| attachments::Failure::AssetUrl { message: message.to_owned() };
    if source.trim().is_empty() || source.len() > SOURCE_LIMIT {
        return Err(err("This diagram is empty or too long to draw."));
    }
    let digest = sha256_hex(format!("svg\0{}\0{source}", if dark { "dark" } else { "light" }).as_bytes());
    if cached(&digest).is_none() {
        let owned = source.to_owned();
        let svg = tokio::task::spawn_blocking(move || std::panic::catch_unwind(|| render_svg(&owned, dark)).ok().flatten())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| err("This diagram couldn’t be rendered."))?;
        remember(digest.clone(), svg);
    }
    let expires_at = crate::time_fmt::now_millis() + TTL_MS;
    let claims = Claims { kind: KIND_SVG.into(), digest, expires_at };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign the URL."))?;
    Ok(AssetCreateUrlResult {
        relative_url: format!("{ROUTE_PREFIX_SVG}/{token}"),
        expires_at: expires_at.into(),
        source_path: None,
        image_dimensions: None,
        file_revision: None,
    })
}

/// The SVG behind a URL `issue_svg` handed out, while it is valid and kept.
pub fn read_svg(userdata: &Path, token: &str) -> Option<Vec<u8>> {
    let claims: Claims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != KIND_SVG || claims.expires_at <= crate::time_fmt::now_millis() {
        return None;
    }
    cached(&claims.digest)
}

fn render_svg(source: &str, dark: bool) -> Option<Vec<u8>> {
    let theme = if dark { "dark" } else { "default" };
    let config = format!(r#"{{"securityLevel":"strict","htmlLabels":false,"maxTextSize":{SOURCE_LIMIT},"maxEdges":500,"theme":"{theme}"}}"#);
    let site_config = SourcePresentationConfig::from_value(serde_json::from_str(&config).expect("valid built-in Mermaid settings"));
    let renderer = Renderer::new()
        .with_presentation_engine(MermaidEngine::new().with_site_config(site_config))
        .with_parse_options(merman::ParseOptions::strict());
    let control = OperationControl::new().with_deadline(Duration::from_secs(2));
    match renderer.render(RenderRequest::svg(source, control, SvgRequest::default())) {
        Ok(RenderOutput::Svg(Some(output))) => Some(output.svg().as_bytes().to_vec()),
        other => {
            tracing::debug!("Mermaid SVG render: {other:?}");
            None
        }
    }
}

fn render(source: &str, dark: bool) -> Option<Vec<u8>> {
    // Merman accepts the upstream configuration document at this boundary.
    let theme = if dark { "dark" } else { "default" };
    let config = format!(r#"{{"securityLevel":"strict","htmlLabels":false,"maxTextSize":{SOURCE_LIMIT},"maxEdges":500,"theme":"{theme}"}}"#);
    let site_config = SourcePresentationConfig::from_value(serde_json::from_str(&config).expect("valid built-in Mermaid settings"));
    let renderer = Renderer::new()
        .with_presentation_engine(MermaidEngine::new().with_site_config(site_config))
        .with_parse_options(merman::ParseOptions::strict());
    let control = OperationControl::new().with_deadline(Duration::from_secs(2));
    static FONTS: OnceLock<Option<NativeFontContext>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| NativeFontContext::from_font_bytes(bundled_font_data(), NativeFontLimits::default(), &OperationControl::new()).ok())
        .as_ref()?;
    let options = RasterOptions::default()
        .with_font_context(fonts.clone())
        .with_fit_to(RasterFitBox::contain(1600, 1400))
        .with_matte(if dark { "#1c1c20" } else { "#ffffff" });
    match renderer.render(RenderRequest::png(source, control, PngRequest { svg: Default::default(), options })) {
        Ok(RenderOutput::Png(Some(output))) => Some(output.bytes().to_vec()),
        other => {
            tracing::debug!("Mermaid render: {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_diagrams_render() {
        for source in [
            "flowchart LR\nsubgraph API\nA[Request café] --> B[Response]\nend",
            "sequenceDiagram\nA->>B: Request\nNote over B: Response",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Working",
            "erDiagram\nCUSTOMER ||--o{ ORDER : places",
        ] {
            for dark in [false, true] {
                assert!(render(source, dark).is_some_and(|png| png.starts_with(b"\x89PNG\r\n\x1a\n")), "{source}");
            }
        }
    }

    #[test]
    fn unsupported_source_does_not_render() {
        assert!(render("not a Mermaid diagram", false).is_none());
    }

    #[tokio::test]
    async fn issued_url_serves_the_png_until_it_expires() {
        let dir = std::env::temp_dir().join(format!("domovoi-diagrams-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let asset = issue(&dir, "flowchart LR\nA --> B", true).await.expect("rendered");
        let token = asset.relative_url.strip_prefix(&format!("{ROUTE_PREFIX}/")).expect("diagram route");
        assert!(read(&dir, token).is_some_and(|png| png.starts_with(b"\x89PNG")));
        assert!(read(&dir, &format!("{token}x")).is_none());
        let svg_asset = issue_svg(&dir, "flowchart LR\nA --> B", false).await.expect("svg rendered");
        let svg_token = svg_asset.relative_url.strip_prefix(&format!("{ROUTE_PREFIX_SVG}/")).expect("svg route");
        assert!(read_svg(&dir, svg_token).is_some_and(|svg| String::from_utf8_lossy(&svg).contains("<svg")));
        assert!(issue(&dir, &"a".repeat(SOURCE_LIMIT + 1), false).await.is_err());
        assert!(issue(&dir, "not a Mermaid diagram", false).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
