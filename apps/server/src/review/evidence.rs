//! Deterministic evidence verification and unified-diff hunk anchoring.
//!
//! Every finding, suggestion, and question MUST carry verbatim code quotes
//! anchored to real lines in the frozen snapshot (allowing ±3 line slack for
//! line-counting drift, failing on ties or mismatches). Every inline finding
//! MUST anchor to an actual changed line in `PR_DIFF.patch`.

use crate::review::published_text::sanitize_markdown;
use crate::review::snapshot::safe_rel_path;
use domovoi_model::typed::{PrReviewEvidenceRef, PrReviewFinding};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Maximum line drift tolerated when rebinding a verbatim quote.
pub const QUOTE_LINE_SLACK: usize = 3;

#[derive(Clone, Debug, Default)]
pub struct FileHunks {
    pub new_lines: HashSet<usize>,
    pub old_lines: HashSet<usize>,
}

/// Parses a unified diff (`PR_DIFF.patch`) into exact valid `new` and `old` line sets per file.
pub fn parse_diff_hunks(diff_text: &str) -> HashMap<String, FileHunks> {
    let mut map: HashMap<String, FileHunks> = HashMap::new();
    let mut current_path: Option<String> = None;
    let mut old_line: usize = 0;
    let mut new_line: usize = 0;
    let mut in_hunk = false;

    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let trimmed = rest.trim();
            if trimmed == "/dev/null" {
                // Keep path from `--- a/...` for deleted files.
            } else {
                let p = trimmed.strip_prefix("b/").unwrap_or(trimmed).to_owned();
                current_path = Some(p);
            }
            in_hunk = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let trimmed = rest.trim();
            if trimmed != "/dev/null" {
                let p = trimmed.strip_prefix("a/").unwrap_or(trimmed).to_owned();
                current_path = Some(p);
            }
            in_hunk = false;
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some((o, n)) = parse_hunk_header(line) {
                old_line = o;
                new_line = n;
                in_hunk = true;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let Some(path) = current_path.as_ref() else { continue };
        let entry = map.entry(path.clone()).or_default();
        if line.starts_with('+') && !line.starts_with("+++") {
            entry.new_lines.insert(new_line);
            new_line += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            entry.old_lines.insert(old_line);
            old_line += 1;
        } else if line.starts_with(' ') {
            // Context lines within a hunk are also commentable in GitHub/GitLab reviews.
            entry.old_lines.insert(old_line);
            entry.new_lines.insert(new_line);
            old_line += 1;
            new_line += 1;
        }
    }
    map
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    // Format: `@@ -old_start[,old_len] +new_start[,new_len] @@`
    let inner = line.strip_prefix("@@ ")?.split(" @@").next()?;
    let mut parts = inner.split_whitespace();
    let old_part = parts.next()?.strip_prefix('-')?;
    let new_part = parts.next()?.strip_prefix('+')?;
    let old_start = old_part.split(',').next()?.parse::<usize>().ok()?;
    let new_start = new_part.split(',').next()?.parse::<usize>().ok()?;
    Some((old_start, new_start))
}

/// Checks whether `(start..=end)` overlaps at least one real hunk line in `hunks` (used by `review_guides.rs` too).
pub fn validate_hunk_range(hunks: &HashMap<String, FileHunks>, path: &str, side: &str, start: u64, end: u64) -> bool {
    let Some(file) = hunks.get(path) else { return false };
    let set = if side == "old" { &file.old_lines } else { &file.new_lines };
    (start as usize..=end as usize).any(|l| set.contains(&l))
}

/// Finds the unique nearest line within `±QUOTE_LINE_SLACK` of `line` containing `quote`.
pub fn quoted_line(content: &str, line: usize, quote: &str) -> Option<usize> {
    let quote = quote.trim();
    if quote.is_empty() {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let holds = |n: usize| n >= 1 && lines.get(n - 1).is_some_and(|l| l.contains(quote));
    if holds(line) {
        return Some(line);
    }
    for dist in 1..=QUOTE_LINE_SLACK {
        let up = line.checked_sub(dist).filter(|n| holds(*n));
        let down = Some(line + dist).filter(|n| holds(*n));
        match (up, down) {
            (Some(n), None) | (None, Some(n)) => return Some(n),
            (Some(_), Some(_)) => return None, // Ambiguous tie fails validation.
            (None, None) => {}
        }
    }
    None
}

/// Raw structured output emitted by the AI reviewer CLI.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReviewReport {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ModelPoint>,
    #[serde(default)]
    pub suggestions: Vec<ModelPoint>,
    #[serde(default)]
    pub questions: Vec<ModelQuestion>,
    #[serde(default)]
    pub reviewed_files: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPoint {
    pub title: String,
    pub detail: String,
    pub action: String,
    #[serde(default = "default_medium")]
    pub severity: String,
    pub path: String,
    pub line: u64,
    #[serde(default = "default_new_side")]
    pub side: String,
    #[serde(default)]
    pub evidence: Vec<PrReviewEvidenceRef>,
    #[serde(default)]
    pub counter_evidence: Vec<PrReviewEvidenceRef>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelQuestion {
    pub title: String,
    pub detail: String,
    pub action: String,
    #[serde(default)]
    pub evidence: Vec<PrReviewEvidenceRef>,
    #[serde(default)]
    pub counter_evidence: Vec<PrReviewEvidenceRef>,
}

fn default_medium() -> String {
    "medium".into()
}
fn default_new_side() -> String {
    "new".into()
}

#[derive(Clone, Debug)]
pub struct ValidatedReport {
    pub summary: String,
    pub findings: Vec<PrReviewFinding>,
}

#[derive(Clone, Debug)]
pub struct ValidationError {
    pub repairable: bool,
    pub message: String,
}

fn read_snapshot_file(snapshot_dir: &Path, rel: &str, side: &str) -> Result<String, ValidationError> {
    let clean = safe_rel_path(rel).map_err(|e| ValidationError { repairable: false, message: e })?;
    let order = if side == "old" { ["BASE", "CHANGED", "RELATED", "CONVENTIONS"] } else { ["CHANGED", "RELATED", "CONVENTIONS", "BASE"] };
    for bucket in order {
        let p = snapshot_dir.join(bucket).join(clean);
        if p.is_file() {
            return fs::read_to_string(&p).map_err(|e| ValidationError { repairable: false, message: e.to_string() });
        }
    }
    Err(ValidationError {
        repairable: false,
        message: format!("Evidence path `{rel}` does not exist in the frozen review snapshot."),
    })
}

fn verify_refs(refs: &[PrReviewEvidenceRef], snapshot_dir: &Path, side: &str, required: bool) -> Result<Vec<PrReviewEvidenceRef>, ValidationError> {
    if required && refs.is_empty() {
        return Err(ValidationError {
            repairable: false,
            message: "Every finding, suggestion, and question must include at least one verbatim code evidence reference.".into(),
        });
    }
    let mut verified = Vec::with_capacity(refs.len());
    for r in refs {
        let content = read_snapshot_file(snapshot_dir, &r.path, side)?;
        let line = quoted_line(&content, r.line as usize, &r.quote).ok_or_else(|| ValidationError {
            repairable: true,
            message: format!("Evidence quote does not match `{}` near line {} (within ±{QUOTE_LINE_SLACK} lines).", r.path, r.line),
        })?;
        verified.push(PrReviewEvidenceRef {
            path: r.path.clone(),
            line: line as u64,
            quote: sanitize_markdown(r.quote.trim()),
        });
    }
    Ok(verified)
}

fn normalize_severity(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "high" | "critical" | "error" => "high".into(),
        "low" | "minor" | "info" => "low".into(),
        _ => "medium".into(),
    }
}

fn normalize_side(raw: &str) -> &'static str {
    if raw.trim().eq_ignore_ascii_case("old") || raw.trim().eq_ignore_ascii_case("left") {
        "old"
    } else {
        "new"
    }
}

/// Validates and sanitizes `report` against the frozen snapshot and diff hunks.
pub fn validate_report(
    run_id: &str,
    stage: &str,
    report: &ModelReviewReport,
    snapshot_dir: &Path,
    hunks: &HashMap<String, FileHunks>,
) -> Result<ValidatedReport, ValidationError> {
    let mut out = Vec::new();

    for f in &report.findings {
        if f.title.trim().is_empty() || f.detail.trim().is_empty() {
            return Err(ValidationError { repairable: false, message: "Finding is missing title or detail.".into() });
        }
        let side = normalize_side(&f.side);
        let ev = verify_refs(&f.evidence, snapshot_dir, side, true)?;
        let cev = verify_refs(&f.counter_evidence, snapshot_dir, side, false)?;

        // Anchor inline finding line to a real changed hunk line (allow rebinding from primary evidence if same file).
        let file_hunks = hunks.get(&f.path).ok_or_else(|| ValidationError {
            repairable: true,
            message: format!("Finding `{}` references `{}` which has no changed diff hunks.", f.title, f.path),
        })?;
        let valid_lines = if side == "old" { &file_hunks.old_lines } else { &file_hunks.new_lines };
        let anchor_line = if valid_lines.contains(&(f.line as usize)) {
            f.line
        } else if let Some(ev_same) = ev.iter().find(|r| r.path == f.path && valid_lines.contains(&(r.line as usize))) {
            ev_same.line
        } else {
            // Try nearest valid hunk line within ±QUOTE_LINE_SLACK.
            let mut rebound = None;
            for d in 1..=QUOTE_LINE_SLACK {
                let up = (f.line as usize).checked_sub(d).filter(|l| valid_lines.contains(l));
                let dn = Some(f.line as usize + d).filter(|l| valid_lines.contains(l));
                match (up, dn) {
                    (Some(l), None) | (None, Some(l)) => {
                        rebound = Some(l as u64);
                        break;
                    }
                    _ => {}
                }
            }
            rebound.ok_or_else(|| ValidationError {
                repairable: true,
                message: format!("Finding `{}` anchors to `{}:{} ({side})` which is outside any changed diff hunk.", f.title, f.path, f.line),
            })?
        };

        out.push(PrReviewFinding {
            id: crate::util::new_uuid(),
            run_id: run_id.to_owned(),
            kind: "finding".into(),
            severity: normalize_severity(&f.severity),
            title: sanitize_markdown(f.title.trim()),
            detail: sanitize_markdown(f.detail.trim()),
            action: sanitize_markdown(f.action.trim()),
            path: Some(f.path.clone()),
            line: Some(anchor_line),
            side: Some(side.to_owned()),
            evidence: ev,
            counter_evidence: cev,
            selected: true,
            source_stage: stage.to_owned(),
        });
    }

    for s in &report.suggestions {
        if s.title.trim().is_empty() {
            continue;
        }
        let side = normalize_side(&s.side);
        let ev = verify_refs(&s.evidence, snapshot_dir, side, true)?;
        let cev = verify_refs(&s.counter_evidence, snapshot_dir, side, false)?;
        out.push(PrReviewFinding {
            id: crate::util::new_uuid(),
            run_id: run_id.to_owned(),
            kind: "suggestion".into(),
            severity: "low".into(),
            title: sanitize_markdown(s.title.trim()),
            detail: sanitize_markdown(s.detail.trim()),
            action: sanitize_markdown(s.action.trim()),
            path: Some(s.path.clone()),
            line: Some(s.line),
            side: Some(side.to_owned()),
            evidence: ev,
            counter_evidence: cev,
            selected: false,
            source_stage: stage.to_owned(),
        });
    }

    for q in &report.questions {
        if q.title.trim().is_empty() {
            continue;
        }
        let ev = verify_refs(&q.evidence, snapshot_dir, "new", true)?;
        let cev = verify_refs(&q.counter_evidence, snapshot_dir, "new", false)?;
        out.push(PrReviewFinding {
            id: crate::util::new_uuid(),
            run_id: run_id.to_owned(),
            kind: "question".into(),
            severity: "low".into(),
            title: sanitize_markdown(q.title.trim()),
            detail: sanitize_markdown(q.detail.trim()),
            action: sanitize_markdown(q.action.trim()),
            path: None,
            line: None,
            side: None,
            evidence: ev,
            counter_evidence: cev,
            selected: false,
            source_stage: stage.to_owned(),
        });
    }

    Ok(ValidatedReport { summary: sanitize_markdown(report.summary.trim()), findings: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinds_quote_within_slack_rejects_ties_and_anchors_to_diff_hunks() {
        let dir = std::env::temp_dir().join(format!("domovoi-ev-{}", crate::util::new_uuid()));
        fs::create_dir_all(dir.join("CHANGED")).unwrap();
        fs::write(dir.join("CHANGED/a.rs"), "line1\nlet x = unwrap();\nline3\nlet dup = 1;\nmid\nlet dup = 1;\n").unwrap();
        let diff = "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,3 @@\n line1\n+let x = unwrap();\n line3\n";
        let hunks = parse_diff_hunks(diff);
        assert!(hunks["a.rs"].new_lines.contains(&2));
        assert!(!hunks["a.rs"].new_lines.contains(&6));

        // Exact + ±2 line slack succeeds and rebinds to line 2.
        let report = ModelReviewReport {
            summary: "ok".into(),
            findings: vec![ModelPoint {
                title: "Unchecked unwrap".into(),
                detail: "Can panic".into(),
                action: "Use ?".into(),
                severity: "high".into(),
                path: "a.rs".into(),
                line: 4, // off by 2, rebounds via evidence to line 2!
                side: "new".into(),
                evidence: vec![PrReviewEvidenceRef { path: "a.rs".into(), line: 4, quote: "unwrap()".into() }],
                counter_evidence: vec![],
            }],
            suggestions: vec![],
            questions: vec![],
            reviewed_files: vec!["a.rs".into()],
        };
        let validated = validate_report("r1", "primary", &report, &dir, &hunks).unwrap();
        assert_eq!(validated.findings[0].line, Some(2));
        assert_eq!(validated.findings[0].evidence[0].line, 2);

        // Tie at line 5 (line 4 and line 6 both match `let dup = 1;`) fails validation.
        let content = fs::read_to_string(dir.join("CHANGED/a.rs")).unwrap();
        assert_eq!(quoted_line(&content, 5, "let dup = 1;"), None);

        // Missing evidence fails as non-repairable.
        let bad = ModelReviewReport {
            findings: vec![ModelPoint {
                title: "No proof".into(),
                detail: "Guess".into(),
                action: "Fix".into(),
                severity: "low".into(),
                path: "a.rs".into(),
                line: 2,
                side: "new".into(),
                evidence: vec![],
                counter_evidence: vec![],
            }],
            ..Default::default()
        };
        assert!(!validate_report("r1", "primary", &bad, &dir, &hunks).unwrap_err().repairable);

        let _ = fs::remove_dir_all(&dir);
    }
}
