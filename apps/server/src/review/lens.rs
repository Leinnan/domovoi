//! Optional second-pass adversarial "Lens" review over primary validated findings.
//!
//! Focuses specifically on high-impact missed defects (concurrency, auth/permission
//! boundaries, error paths, data loss, contract mismatches) without duplicating
//! findings already raised in the primary pass.

use domovoi_model::typed::PrReviewFinding;
use std::collections::HashSet;

pub fn build_lens_prompt(base_prompt: &str, primary_findings: &[PrReviewFinding]) -> String {
    let existing = serde_json::to_string_pretty(primary_findings).unwrap_or_else(|_| "[]".into());
    format!(
        "{base_prompt}\n\n\
         ## Second-Pass Adversarial Lens Instructions\n\
         A primary review pass has already run and produced the validated findings below.\n\
         Do NOT repeat or rephrase any of those findings.\n\
         Re-examine the diff and surrounding files in the frozen snapshot through a focused adversarial lens:\n\
         - Auth, authorization, path traversal, injection, secret exposure\n\
         - Concurrency, race conditions, lock ordering, partial-failure / rollback gaps\n\
         - Boundary conditions, silent error swallowing, schema/wire compatibility breaks\n\
         - Missing or weakened tests for newly introduced risky behavior\n\
         Return ONLY genuinely new, evidence-backed findings (or empty arrays if nothing material was missed).\n\n\
         Existing primary findings (do not duplicate):\n{existing}"
    )
}

/// Merges newly validated `lens` findings into `primary`, deduplicating by `(path, line, normalized_title)`.
pub fn merge_unique(mut primary: Vec<PrReviewFinding>, lens: Vec<PrReviewFinding>) -> (Vec<PrReviewFinding>, usize) {
    let mut seen: HashSet<(Option<String>, Option<u64>, String)> = primary
        .iter()
        .map(|f| (f.path.clone(), f.line, f.title.trim().to_ascii_lowercase()))
        .collect();
    let mut added = 0usize;
    for item in lens {
        let key = (item.path.clone(), item.line, item.title.trim().to_ascii_lowercase());
        if seen.insert(key) {
            primary.push(item);
            added += 1;
        }
    }
    (primary, added)
}
