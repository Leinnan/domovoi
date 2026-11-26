//! `html_preview` and `html_render`. `html_preview`
//! loads the page in the server's headless browser (`crate::preview::host`)
//! at a private origin, in a fresh browser context, and answers its PNG
//! (image content), content height, captured height (at most 4,000 px) and
//! console output, as t3code's `HtmlRender.preview`. `html_render` shows a
//! finished page in the caller's thread: Domovoi publishes it as an `html`
//! artifact (`crate::artifacts`), which its clients show in the thread's
//! Artifacts panel; `htmlRender.attachmentId` is the artifact's id.
//!
//! Only a full-access thread may pass a `path`: the server reads the file
//! for the caller, which a supervised thread's provider has not let it read.
//!
//! Limits: local images are not inlined (no `missingImages`) and `vh` units
//! are not refused; `html_preview` applies no T3 Code theme fragment; the
//! rendered page is not measured, so `heights` is never given.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, FULL_RESULT, n, one_of, s, tool};
use serde_json::{Value, json};

const MIN_HEIGHT: u64 = 80;
const MAX_HEIGHT: u64 = 2000;
const MAX_TITLE_CHARS: usize = 200;
const MAX_PAGE_BYTES: usize = 512_000;
/// `HTML_RENDER_COLUMN_WIDTH`, and the widths `html_preview` takes.
const COLUMN_WIDTH: u64 = 728;
const MIN_PREVIEW_WIDTH: u64 = 240;
const MAX_PREVIEW_WIDTH: u64 = 1_600;

pub fn tools() -> Vec<Value> {
    vec![tool(
        "html_preview",
        "Check an HTML page before html_render. Returns PNG, contentHeight, and console output. Self-contained: inline <style> and <script>. Remote URLs load as-is. Theme and layout rules are in html_render.",
        json!({
            "path": s("Absolute .html file path (full-access threads only)."),
            "html": s("Inline HTML."),
            "width": n("CSS pixels, 240 to 1600. Default 728. Use about 390 for phones."),
            "appearance": one_of(&["dark", "light"], "Default dark."),
        }),
        &[],
        true,
    ), tool(
        "html_render",
        "Show a finished HTML page (chart, diagram, mockup) in this thread, as an HTML artifact. Do not announce or restate the page. Self-contained: inline <style> and <script>; remote URLs load as-is. Prefer CSS that follows `prefers-color-scheme`; no `vh` heights.",
        json!({
            "path": s("Absolute .html file path (full-access threads only)."),
            "html": s("Inline HTML."),
            "title": s("Short title, at most 200 characters."),
            "height": n("Frame height in CSS pixels, 80 to 2000."),
        }),
        &["title", "height"],
        true,
    )]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "html_render" => render(c),
        "html_preview" => preview(c).await,
        _ => return None,
    })
}

/// The page: inline, or (for a full-access thread) read from an absolute `.html` file.
fn page(c: &Call<'_>) -> Result<String, String> {
    let html = c.args["html"].as_str().filter(|h| !h.is_empty());
    let path = c.str("path");
    let text = match (html, path) {
        (Some(html), None) => html.to_owned(),
        (None, Some(path)) => {
            if c.caller.thread.runtime_mode.as_str() != "full-access" {
                return Err("capability_denied: Pass html instead of path: only full-access threads may pass a path.".into());
            }
            let file = std::path::Path::new(&path);
            let real = std::fs::canonicalize(file).map_err(|_| format!("invalid_request: Could not read {path}."))?;
            let is_html = real.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
            if !file.is_absolute() || !is_html || !real.is_file() {
                return Err(format!("invalid_request: {path} is not an absolute path to an .html file."));
            }
            if std::fs::metadata(&real).map(|m| m.len() as usize > MAX_PAGE_BYTES).unwrap_or(true) {
                return Err(format!("invalid_request: {path} is larger than {MAX_PAGE_BYTES} bytes."));
            }
            std::fs::read_to_string(&real).map_err(|_| format!("invalid_request: {path} is not UTF-8 text."))?
        }
        _ => return Err("invalid_request: Pass exactly one of html or path.".into()),
    };
    if text.len() > MAX_PAGE_BYTES {
        return Err(format!("invalid_request: the page is larger than {MAX_PAGE_BYTES} bytes."));
    }
    Ok(text)
}

/// `html_preview`: metadata as JSON text, then the PNG as image content.
async fn preview(c: &Call<'_>) -> Result<Value, String> {
    let html = page(c)?;
    let width = c.args["width"].as_f64().map_or(COLUMN_WIDTH, |w| (w.round().max(0.0) as u64).clamp(MIN_PREVIEW_WIDTH, MAX_PREVIEW_WIDTH));
    let dark = c.args["appearance"].as_str() != Some("light");
    crate::preview::host::start(c.state);
    let shot = crate::preview::host::html_preview(c.state, html, width as u32, dark).await.map_err(|e| format!("HTML preview failed: {e}"))?;
    let metadata = json!({
        "width": width,
        "contentHeight": shot.content_height,
        "capturedHeight": shot.captured_height,
        "consoleMessages": shot.console.iter().map(|(level, text)| json!({ "level": level, "text": text })).collect::<Vec<_>>(),
        "screenshot": { "mimeType": "image/png", "width": width, "height": shot.captured_height },
    });
    Ok(json!({ FULL_RESULT: {
        "isError": false,
        "structuredContent": metadata,
        "content": [{ "type": "text", "text": metadata.to_string() }, { "type": "image", "data": shot.png, "mimeType": "image/png" }],
    } }))
}

fn render(c: &Call<'_>) -> Result<Value, String> {
    let title: String = c.required("title")?.chars().take(MAX_TITLE_CHARS).collect();
    let height = c.args["height"].as_u64().ok_or("height is required.")?.clamp(MIN_HEIGHT, MAX_HEIGHT);
    let html = page(c)?;
    let (artifact, _) = crate::artifacts::upsert(
        c.store(),
        crate::artifacts::UpsertInput {
            id: None,
            thread_id: &c.caller.thread_id,
            project_id: &c.caller.project_id,
            title: &title,
            kind: "html",
            language: Some("html"),
            source_path: None,
            hosted_url: None,
            run_id: None,
            turn_item_id: None,
            summary: None,
            content: &html,
        },
    )?;
    Ok(json!({
        "htmlRender": { "attachmentId": artifact.id, "title": artifact.title, "height": height },
        "message": "Shown to the reader in this thread. Don't mention or describe the page; reply with only what it doesn't already say.",
    }))
}
