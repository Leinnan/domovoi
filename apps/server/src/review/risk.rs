//! Blast-radius and sensitive-surface risk scoring for pull requests.

use crate::indexer::query::IndexContext;
use domovoi_model::typed::PrReviewRiskDriver;

pub fn score_risk(reviewable_files: &[String], diff_text: &str, index_ctx: &IndexContext) -> (u32, Vec<PrReviewRiskDriver>) {
    let mut score: i64 = 0;
    let mut drivers = Vec::new();

    let push = |drivers: &mut Vec<PrReviewRiskDriver>, score: &mut i64, points: i64, label: &str, detail: String| {
        if points > 0 {
            *score += points;
            drivers.push(PrReviewRiskDriver { points, label: label.to_owned(), detail });
        }
    };

    // 1. File spread.
    let changed = reviewable_files.len();
    let file_pts = match changed {
        n if n >= 25 => 22,
        n if n >= 12 => 14,
        n if n >= 6 => 8,
        n if n >= 3 => 4,
        _ => 0,
    };
    push(&mut drivers, &mut score, file_pts, "File spread", format!("{changed} reviewable files"));

    // 2. Diff volume.
    let changed_lines = diff_text
        .lines()
        .filter(|l| (l.starts_with('+') && !l.starts_with("+++")) || (l.starts_with('-') && !l.starts_with("---")))
        .count();
    let diff_pts = match changed_lines {
        n if n >= 1000 => 20,
        n if n >= 400 => 12,
        n if n >= 150 => 6,
        _ => 0,
    };
    push(&mut drivers, &mut score, diff_pts, "Diff size", format!("{changed_lines} added/removed lines"));

    // 3. Blast radius from the codebase index.
    let dep_pkgs = index_ctx.dependent_packages.len() as i64;
    let related = index_ctx.related_files.len() as i64;
    let blast_pts = (dep_pkgs * 4 + (related / 4)).min(22);
    push(
        &mut drivers,
        &mut score,
        blast_pts,
        "Blast radius",
        format!("{dep_pkgs} dependent packages, {related} related callers/tests"),
    );

    // 4. Sensitive paths (auth, permissions, crypto, migrations, workflows, webhooks).
    let sensitive: Vec<&String> = reviewable_files
        .iter()
        .filter(|p| {
            let l = p.to_ascii_lowercase();
            l.contains("auth")
                || l.contains("permission")
                || l.contains("secret")
                || l.contains("crypto")
                || l.contains("security")
                || l.contains("migration")
                || l.contains("db.rs")
                || l.contains("webhook")
                || l.contains(".github/workflows/")
        })
        .collect();
    let sens_pts = ((sensitive.len() as i64) * 7).min(24);
    push(
        &mut drivers,
        &mut score,
        sens_pts,
        "Sensitive surface",
        format!("{} sensitive files touched", sensitive.len()),
    );

    // 5. Code changes without accompanying test changes.
    let code_count = reviewable_files.iter().filter(|p| !crate::indexer::is_test_path(p) && crate::indexer::detect_language(p).is_some()).count();
    let test_count = reviewable_files.iter().filter(|p| crate::indexer::is_test_path(p)).count();
    if code_count >= 2 && test_count == 0 {
        push(&mut drivers, &mut score, 12, "Test coverage", format!("{code_count} code files changed with no test files touched"));
    }

    drivers.sort_by(|a, b| b.points.cmp(&a.points));
    (score.clamp(0, 100) as u32, drivers)
}
