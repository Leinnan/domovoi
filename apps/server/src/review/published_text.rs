//! Secret redaction and Markdown/HTML neutralization for review text before
//! persistence or GitHub/GitLab publication.

use regex::Regex;
use std::sync::OnceLock;

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)(?:sk-[a-z0-9_-]{20,})",
            r"(?i)(?:gh[pousr]_[a-z0-9]{20,})",
            r"(?i)(?:glpat-[a-z0-9_-]{20,})",
            r"(?i)(?:AKIA[0-9A-Z]{16})",
            r"(?i)(?:AIza[0-9A-Za-z_-]{35})",
            r"(?i)(?:-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----)",
            r"(?i)((?:authorization|x-api-key|api[_-]?key|secret|token)\s*[:=]\s*['\x22]?[a-z0-9._/+=-]{16,}['\x22]?)",
        ]
        .into_iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    })
}

/// Replaces credential/secret patterns with `[redacted]`.
pub fn redact_secrets(input: &str) -> String {
    let mut out = input.to_owned();
    for re in secret_patterns() {
        out = re.replace_all(&out, "[redacted]").into_owned();
    }
    out
}

/// Strips control characters and neutralizes raw HTML tags in untrusted text.
pub fn sanitize_markdown(input: &str) -> String {
    let redacted = redact_secrets(input);
    let mut cleaned = String::with_capacity(redacted.len());
    for ch in redacted.chars() {
        if ch.is_control() && !matches!(ch, '\n' | '\t') {
            continue;
        }
        cleaned.push(ch);
    }
    cleaned
        .replace("<!--", "&lt;!--")
        .replace("-->", "--&gt;")
        .replace("<script", "&lt;script")
        .replace("</script", "&lt;/script")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_and_neutralizes_html_comments() {
        let raw = "Leaked ghp_1234567890abcdefghij123456 and <!-- injected --> <script>alert(1)</script>";
        let safe = sanitize_markdown(raw);
        assert!(!safe.contains("ghp_123456"));
        assert!(safe.contains("[redacted]"));
        assert!(!safe.contains("<!--"));
        assert!(!safe.contains("<script"));
    }
}
