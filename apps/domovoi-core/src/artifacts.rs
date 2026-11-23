//! Artifact identification, titling, version grouping, and isolated preview
//! document generation shared across all native shells.

use crate::view::{ArtifactSummaryView, ArtifactVersionView};
use std::collections::HashMap;

pub fn is_html_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// Maps a file path's extension to an artifact kind, if previewable as an artifact.
pub fn artifact_kind_for_path(path: &str) -> Option<&'static str> {
    let (_, ext) = path.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => Some("html"),
        "svg" => Some("svg"),
        "jsx" | "tsx" => Some("react"),
        "mmd" | "mermaid" => Some("mermaid"),
        _ => None,
    }
}

/// Maps a fenced code block language to an artifact kind, if previewable.
pub fn artifact_kind_for_language(lang: &str) -> Option<&'static str> {
    match lang.trim().to_ascii_lowercase().as_str() {
        "html" | "htm" => Some("html"),
        "svg" => Some("svg"),
        "jsx" | "tsx" | "react" => Some("react"),
        "mermaid" | "mmd" => Some("mermaid"),
        _ => None,
    }
}

/// Normalizes an explicit artifact kind string.
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

/// Markdown destinations are URL-encoded, unlike file-change record paths.
#[cfg(test)]
pub fn html_link_path(destination: &str) -> Option<String> {
    let path = destination.split(['?', '#']).next()?;
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    is_html_path(&decoded).then(|| decoded.into_owned())
}

/// Decodes a markdown link destination if it points to any previewable workspace artifact.
pub fn artifact_link_path(destination: &str) -> Option<String> {
    if destination.contains("://") && !destination.starts_with("file://") {
        return None;
    }
    let path = destination.split(['?', '#']).next()?;
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    artifact_kind_for_path(&decoded).map(|_| decoded.into_owned())
}

pub fn is_html_document(source: &str) -> bool {
    let source = source.trim();
    let start = source
        .split_once('>')
        .map_or("", |(prefix, _)| prefix)
        .to_ascii_lowercase();
    let end = source.len().saturating_sub("</html>".len());
    (start == "<html"
        || start.starts_with("<html ")
        || start.split_whitespace().eq(["<!doctype", "html"]))
        && source
            .get(end..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("</html>"))
}

pub fn is_svg_document(source: &str) -> bool {
    let trimmed = source.trim();
    let body = if trimmed.starts_with("<?xml") {
        trimmed.split_once("?>").map_or("", |(_, rest)| rest.trim_start())
    } else {
        trimmed
    };
    let lower_start = body.get(..4).map(str::to_ascii_lowercase);
    let end = body.len().saturating_sub("</svg>".len());
    lower_start.as_deref() == Some("<svg")
        && body
            .get(end..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("</svg>"))
}

/// Extracts a human-readable title for an artifact from its source or path.
pub fn extract_title(kind: &str, source: &str, fallback_path: Option<&str>) -> String {
    if let Some(title) = extract_title_from_source(kind, source) {
        return title;
    }
    if let Some(path) = fallback_path.map(str::trim).filter(|p| !p.is_empty()) {
        return path.rsplit('/').next().unwrap_or(path).to_owned();
    }
    match kind {
        "html" => "HTML artifact",
        "svg" => "SVG graphic",
        "react" => "React component",
        "mermaid" => "Mermaid diagram",
        "markdown" => "Markdown document",
        "hosted" => "Claude artifact",
        _ => "Code artifact",
    }
    .to_owned()
}

fn extract_title_from_source(kind: &str, source: &str) -> Option<String> {
    match kind {
        "html" | "svg" => {
            let lower = source.to_ascii_lowercase();
            let start = lower.find("<title>")? + "<title>".len();
            let end = lower[start..].find("</title>")? + start;
            let raw = source.get(start..end)?.trim();
            clean_title(raw)
        }
        "react" => {
            for line in source.lines().take(60) {
                let t = line.trim();
                for prefix in [
                    "export default function ",
                    "export function ",
                    "function ",
                    "export default const ",
                    "const ",
                ] {
                    if let Some(rest) = t.strip_prefix(prefix) {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                            return Some(name);
                        }
                    }
                }
            }
            None
        }
        "markdown" => {
            for line in source.lines().take(20) {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("# ") {
                    return clean_title(rest);
                }
            }
            None
        }
        _ => None,
    }
}

fn clean_title(raw: &str) -> Option<String> {
    let single: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = single.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(80).collect())
}

fn normalize_workspace_path(root: &str, path: &str, allowed: fn(&str) -> bool) -> Option<String> {
    let root = root.replace('\\', "/");
    let path = path.replace('\\', "/");
    let path = path.strip_prefix("file://").unwrap_or(&path);
    let relative = if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        path.strip_prefix(root.trim_end_matches('/'))?
            .strip_prefix('/')?
    } else {
        path
    };
    if root.is_empty() || !allowed(relative) || relative.contains([':', '?', '#', '\0']) {
        return None;
    }
    let mut components = Vec::new();
    for segment in relative.split('/') {
        match segment {
            ".." => return None,
            "" | "." => {}
            component => components.push(component),
        }
    }
    Some(components.join("/"))
}

#[boltffi::export]
pub fn workspace_html_path(root: &str, path: &str) -> Option<String> {
    normalize_workspace_path(root, path, is_html_path)
}

#[boltffi::export]
pub fn workspace_artifact_path(root: &str, path: &str) -> Option<String> {
    normalize_workspace_path(root, path, |p| artifact_kind_for_path(p).is_some())
}

fn attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wraps an SVG document into a centered, theme-aware HTML preview document.
fn wrap_svg(source: &str, dark: bool) -> String {
    let bg = if dark { "#18181b" } else { "#ffffff" };
    let fg = if dark { "#f4f4f5" } else { "#18181b" };
    format!(
        r#"<html><head><style>
html,body{{margin:0;padding:16px;min-height:100vh;box-sizing:border-box;display:flex;align-items:center;justify-content:center;background:{bg};color:{fg};}}
svg{{max-width:100%;max-height:calc(100vh - 32px);height:auto;width:auto;display:block;}}
</style></head><body>{source}</body></html>"#
    )
}

#[boltffi::export]
pub fn html_preview_document(source: &str, interactive: bool) -> String {
    artifact_preview_document("html", source, interactive, false)
}

/// Builds the isolated preview HTML document for `"html"` or `"svg"` artifacts.
///
/// - Static mode (`interactive == false`) uses `script-src 'none'`.
/// - Interactive mode (`interactive == true`) mounts the document inside an
///   opaque-origin `<iframe sandbox="allow-scripts">` after verifying
///   `self.origin === 'null'` and blocking WebRTC, and bridges uncaught JS
///   errors read-only to native shells (`domovoiArtifact` / `DomovoiArtifact`).
#[boltffi::export]
pub fn artifact_preview_document(kind: &str, source: &str, interactive: bool, dark: bool) -> String {
    if source.len() > 2 * 1024 * 1024 {
        return String::new();
    }
    let owned_svg;
    let body_source = if kind.eq_ignore_ascii_case("svg") {
        owned_svg = wrap_svg(source, dark);
        owned_svg.as_str()
    } else {
        source
    };
    let script = if interactive {
        "'unsafe-inline'"
    } else {
        "'none'"
    };
    let policy = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src {script}; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'; webrtc 'block'\">"
    );
    let viewport = "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">";
    let bg = if dark { "#18181b" } else { "#ffffff" };
    let fg = if dark { "#f4f4f5" } else { "#18181b" };
    let scheme = if dark { "dark" } else { "light" };
    let theme_style = format!(
        "<style>:root{{color-scheme:{scheme};--domovoi-bg:{bg};--domovoi-fg:{fg};}}html,body{{background:{bg};color:{fg};}}</style>"
    );
    if !interactive {
        // WebKit does not paint srcdoc with content scripts disabled. The
        // dedicated native view disables script execution and navigation.
        return format!("<!doctype html>{policy}{viewport}{theme_style}{body_source}");
    }
    // Inject a read-only error forwarder inside the opaque iframe before the
    // artifact's scripts run; the outer frame forwards it to native shells.
    let error_hook = r#"<script>
(() => {
  const send = (msg, line) => {
    try { parent.postMessage({ __domovoiArtifactError: true, message: String(msg || 'Runtime error'), line: Number(line) || 0 }, '*'); } catch (_) {}
  };
  window.addEventListener('error', e => send(e.message || (e.error && e.error.message), e.lineno));
  window.addEventListener('unhandledrejection', e => send(e.reason && (e.reason.message || String(e.reason)), 0));
  const origErr = console.error;
  console.error = function(...args) {
    if (args.length) send(args.map(a => typeof a === 'string' ? a : (a && a.message) || String(a)).join(' '), 0);
    return origErr ? origErr.apply(console, args) : undefined;
  };
})();
</script>"#;
    let document = format!("{policy}{viewport}{theme_style}{error_hook}{body_source}");
    let encoded = serde_json::to_string(&document)
        .unwrap_or_default()
        .replace('<', "\\u003c");
    let bootstrap = format!(
        r#"<!doctype html>{policy}{viewport}<script>
(() => {{
const opaque = self.origin === 'null';
const hasRTC = ['RTCPeerConnection', 'webkitRTCPeerConnection', 'mozRTCPeerConnection', 'RTCIceTransport']
    .some(name => typeof globalThis[name] !== 'undefined');
if (opaque) {{
    document.open();
    const staticPolicy = '<meta http-equiv="Content-Security-Policy" content="script-src \'none\'">';
    document.write('<!doctype html>' + (hasRTC ? staticPolicy : '') + {encoded});
    document.close();
    if (hasRTC) document.addEventListener('DOMContentLoaded', () => {{
        const notice = document.createElement('p');
        notice.textContent = 'Interactive preview is unavailable on this runtime. The static preview is shown.';
        notice.style.cssText = 'font: 14px system-ui; padding: 0.65rem; background: #fff4d6; color: #3f3000;';
        document.body.prepend(notice);
    }});
}} else {{
    document.addEventListener('DOMContentLoaded', () => {{
        document.body.textContent = 'Interactive preview is unavailable on this runtime. Turn off interactivity for the static view.';
    }});
}}
}})();
</script>"#
    );
    let contents = attribute(&bootstrap);
    format!(
        r#"<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1"><style>
html,body,iframe {{ width:100%; height:100%; margin:0; border:0; display:block; background:{bg}; }}
</style><script>
window.addEventListener('message', (ev) => {{
  const d = ev.data;
  if (!d || d.__domovoiArtifactError !== true) return;
  try {{
    if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.domovoiArtifact) {{
      window.webkit.messageHandlers.domovoiArtifact.postMessage({{ kind: 'error', message: String(d.message || ''), line: Number(d.line) || 0 }});
    }} else if (window.DomovoiArtifact && typeof window.DomovoiArtifact.onError === 'function') {{
      window.DomovoiArtifact.onError(String(d.message || ''), Number(d.line) || 0);
    }}
  }} catch (_) {{}}
}});
</script><iframe title="HTML artifact" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc="{contents}"></iframe>"#
    )
}

// ---- Unified thread artifact collection & version grouping ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedVersion {
    pub version: u32,
    pub item_id: String,
    pub summary: String,
    pub source: Option<String>,
    pub path: Option<String>,
    pub hosted_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedArtifact {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub language: String,
    pub path: Option<String>,
    pub hosted_url: Option<String>,
    pub streaming: bool,
    pub versions: Vec<CollectedVersion>,
}

impl CollectedArtifact {
    pub fn latest(&self) -> Option<&CollectedVersion> {
        self.versions.last()
    }

    pub fn summary_view(&self) -> ArtifactSummaryView {
        let latest = self.latest();
        ArtifactSummaryView {
            id: self.id.clone(),
            title: self.title.clone(),
            kind: self.kind.clone(),
            language: self.language.clone(),
            latest_version: latest.map_or(1, |v| v.version),
            version_count: self.versions.len() as u32,
            item_id: latest.map(|v| v.item_id.clone()).unwrap_or_default(),
            path: self.path.clone(),
            hosted_url: self.hosted_url.clone(),
            streaming: self.streaming,
        }
    }

    pub fn version_views(&self) -> Vec<ArtifactVersionView> {
        self.versions
            .iter()
            .map(|v| ArtifactVersionView {
                version: v.version,
                summary: v.summary.clone(),
                item_id: v.item_id.clone(),
            })
            .collect()
    }
}

/// Accumulates artifacts in chronological order, grouping updates by id/path/title
/// into sequential versions (`v1`, `v2`, ...) and returning newest-updated first.
#[derive(Default)]
pub struct Collector {
    order: Vec<String>,
    by_id: HashMap<String, CollectedArtifact>,
    title_keys: HashMap<String, String>,
}

pub struct EntryInput {
    pub explicit_id: Option<String>,
    pub item_id: String,
    pub title: String,
    pub kind: String,
    pub language: String,
    pub source: Option<String>,
    pub path: Option<String>,
    pub hosted_url: Option<String>,
    pub summary: String,
    pub streaming: bool,
}

impl Collector {
    /// Registers an artifact occurrence and returns `(artifact_id, version_number)`.
    pub fn push(&mut self, input: EntryInput) -> (String, u32) {
        let key = if let Some(id) = input.explicit_id.filter(|s| !s.trim().is_empty()) {
            id
        } else if let Some(path) = input.path.as_deref().filter(|s| !s.trim().is_empty()) {
            format!("file:{path}")
        } else {
            let norm_title = input.title.trim().to_ascii_lowercase();
            let generic = matches!(
                norm_title.as_str(),
                "" | "html artifact" | "svg graphic" | "react component" | "mermaid diagram" | "markdown document" | "code artifact" | "claude artifact"
            );
            if !generic {
                let tkey = format!("{}:{norm_title}", input.kind);
                if let Some(existing) = self.title_keys.get(&tkey) {
                    existing.clone()
                } else {
                    let id = format!("inline:{}", input.item_id);
                    self.title_keys.insert(tkey, id.clone());
                    id
                }
            } else {
                format!("inline:{}", input.item_id)
            }
        };

        if let Some(art) = self.by_id.get_mut(&key) {
            if !input.title.trim().is_empty() {
                art.title = input.title;
            }
            if !input.kind.is_empty() {
                art.kind = input.kind;
            }
            if !input.language.is_empty() {
                art.language = input.language;
            }
            if input.path.is_some() {
                art.path = input.path.clone();
            }
            if input.hosted_url.is_some() {
                art.hosted_url = input.hosted_url.clone();
            }
            art.streaming = input.streaming;
            // Avoid duplicating identical file-only references (e.g. a markdown link
            // referring to a file that was just written).
            let duplicate_ref = input.source.is_none()
                && art.versions.last().is_some_and(|last| last.path == input.path && last.source.is_none());
            let version = if duplicate_ref {
                art.versions.last().map_or(1, |v| v.version)
            } else if art.versions.last().is_some_and(|last| last.item_id == input.item_id) {
                if let Some(last) = art.versions.last_mut() {
                    if input.source.is_some() {
                        last.source = input.source;
                    }
                    if input.hosted_url.is_some() {
                        last.hosted_url = input.hosted_url;
                    }
                    if !input.summary.is_empty() {
                        last.summary = input.summary;
                    }
                }
                art.versions.last().map_or(1, |v| v.version)
            } else {
                let next = art.versions.last().map_or(1, |v| v.version + 1);
                art.versions.push(CollectedVersion {
                    version: next,
                    item_id: input.item_id,
                    summary: input.summary,
                    source: input.source,
                    path: input.path,
                    hosted_url: input.hosted_url,
                });
                next
            };
            // Move to most-recently-updated end.
            self.order.retain(|id| id != &key);
            self.order.push(key.clone());
            (key, version)
        } else {
            let art = CollectedArtifact {
                id: key.clone(),
                title: input.title,
                kind: input.kind,
                language: input.language,
                path: input.path.clone(),
                hosted_url: input.hosted_url.clone(),
                streaming: input.streaming,
                versions: vec![CollectedVersion {
                    version: 1,
                    item_id: input.item_id,
                    summary: input.summary,
                    source: input.source,
                    path: input.path,
                    hosted_url: input.hosted_url,
                }],
            };
            self.order.push(key.clone());
            self.by_id.insert(key.clone(), art);
            (key, 1)
        }
    }

    /// Returns collected artifacts newest-updated first.
    pub fn finish(mut self) -> Vec<CollectedArtifact> {
        self.order
            .into_iter()
            .rev()
            .filter_map(|id| self.by_id.remove(&id))
            .collect()
    }
}

/// Extracts a `https://claude.ai/...` artifact URL from tool output text.
pub fn extract_claude_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | '.'));
        if cleaned.starts_with("https://claude.ai/") || cleaned.starts_with("https://www.claude.ai/") {
            return Some(cleaned.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_must_be_in_the_worktree() {
        assert_eq!(
            workspace_html_path("/tmp/work", "file:///tmp/work/docs/INDEX.HTML").as_deref(),
            Some("docs/INDEX.HTML")
        );
        assert_eq!(
            workspace_html_path("/tmp/work", "./demo.htm").as_deref(),
            Some("demo.htm")
        );
        assert_eq!(
            workspace_artifact_path("/tmp/work", "charts/diagram.svg").as_deref(),
            Some("charts/diagram.svg")
        );
        assert_eq!(
            workspace_artifact_path("/tmp/work", "ui/Widget.tsx").as_deref(),
            Some("ui/Widget.tsx")
        );
        for path in [
            "https://host/index.html",
            "../escape.html",
            "/tmp/work-two/index.html",
            "index.html#anchor",
        ] {
            assert!(workspace_html_path("/tmp/work", path).is_none(), "{path}");
            assert!(workspace_artifact_path("/tmp/work", path).is_none(), "{path}");
        }
    }

    #[test]
    fn workspace_links_decode_url_paths_before_validation() {
        let path = html_link_path("reports/Quarter%20one.HTML#summary").unwrap();
        assert_eq!(
            workspace_html_path("/work", &path).as_deref(),
            Some("reports/Quarter one.HTML")
        );
        let svg_path = artifact_link_path("diagrams/Arch%20v2.svg").unwrap();
        assert_eq!(
            workspace_artifact_path("/work", &svg_path).as_deref(),
            Some("diagrams/Arch v2.svg")
        );
        let traversal = html_link_path("%2e%2e/report.html").unwrap();
        assert!(workspace_html_path("/work", &traversal).is_none());
        assert!(html_link_path("report.pdf").is_none());
    }

    #[test]
    fn full_documents_titling_and_preview_isolation() {
        assert!(is_html_document(
            " <!DOCTYPE html>\n<html><title>Sales Report</title><p>Hi</p></html> "
        ));
        assert!(!is_html_document("A fragment\n<html><p>Hi</p></html>"));
        assert!(is_svg_document("<svg viewBox=\"0 0 10 10\"><title>Flow</title></svg>"));
        assert_eq!(extract_title("html", "<html><head><title>Sales Report</title></head></html>", None), "Sales Report");
        assert_eq!(extract_title("react", "export default function PricingCalc() { return null; }", None), "PricingCalc");
        assert_eq!(extract_title("svg", "<svg><title>System Map</title></svg>", None), "System Map");
        assert_eq!(extract_title("markdown", "# Architecture Notes\nBody", None), "Architecture Notes");

        let source = "</iframe><script>window.example = true</script>";
        let preview = html_preview_document(source, false);
        assert!(preview.starts_with("<!doctype html><meta http-equiv="));
        assert!(preview.contains("script-src 'none'"));
        let interactive = artifact_preview_document("html", source, true, true);
        assert!(interactive.contains("sandbox=\"allow-scripts\""));
        assert!(interactive.contains("self.origin"));
        assert!(interactive.contains("__domovoiArtifactError"));
        assert!(interactive.contains("color-scheme:dark"));
        assert_eq!(interactive.matches("</iframe>").count(), 1);
    }

    #[test]
    fn collector_groups_versions_by_id_path_and_title() {
        let mut c = Collector::default();
        let (id1, v1) = c.push(EntryInput {
            explicit_id: Some("dash".into()),
            item_id: "i1".into(),
            title: "Dashboard".into(),
            kind: "html".into(),
            language: "html".into(),
            source: Some("<html>v1</html>".into()),
            path: None,
            hosted_url: None,
            summary: "Initial".into(),
            streaming: false,
        });
        let (id2, v2) = c.push(EntryInput {
            explicit_id: Some("dash".into()),
            item_id: "i2".into(),
            title: "Dashboard".into(),
            kind: "html".into(),
            language: "html".into(),
            source: Some("<html>v2</html>".into()),
            path: None,
            hosted_url: Some("https://claude.ai/artifact/dash".into()),
            summary: "Added filters".into(),
            streaming: false,
        });
        assert_eq!((id1, v1), ("dash".into(), 1));
        assert_eq!((id2, v2), ("dash".into(), 2));
        let all = c.finish();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].versions.len(), 2);
        assert_eq!(all[0].hosted_url.as_deref(), Some("https://claude.ai/artifact/dash"));
    }
}
