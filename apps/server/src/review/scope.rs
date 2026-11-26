//! Review scope planning: full, incremental, base-sync, and stacked PR diffs,
//! plus deterministic exclusion of generated/lockfile/vendor paths.

use domovoi_model::typed::{PrReviewFinding, PrReviewResult};
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewScope {
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExcludedFile {
    pub path: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct ScopePlan {
    /// `full` | `incremental` | `base_sync` | `stacked`
    pub mode: String,
    pub source_run_id: Option<String>,
    pub merge_base_sha: String,
    /// Commit against which the diff for this run is generated (`merge_base_sha` for full, `prev_head` for incremental).
    pub diff_base_sha: String,
    pub reviewable_files: Vec<String>,
    pub excluded_files: Vec<ExcludedFile>,
    /// Findings carried forward from untouched files in incremental mode.
    pub reused_findings: Vec<PrReviewFinding>,
}

pub fn git_capture(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::process::blocking_command("git");
    cmd.current_dir(repo).args(args);
    let out = crate::process::output_blocking(&mut cmd, crate::process::QUICK).map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

pub fn merge_base(repo: &Path, base: &str, head: &str) -> Result<String, String> {
    let sha = git_capture(repo, &["merge-base", base, head])?.trim().to_owned();
    if sha.is_empty() {
        return Err("Could not compute merge-base for pull request.".into());
    }
    Ok(sha)
}

pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    let mut cmd = crate::process::blocking_command("git");
    cmd.current_dir(repo).args(["merge-base", "--is-ancestor", ancestor, descendant]);
    crate::process::output_blocking(&mut cmd, crate::process::QUICK).is_ok_and(|o| o.status.success())
}

/// Returns `true` when two scopes cover the exact same PR commit and merge-base.
pub fn equivalent(repo: &Path, previous: &ReviewScope, current: &ReviewScope) -> bool {
    if previous.head_sha != current.head_sha || previous.base_ref != current.base_ref {
        return false;
    }
    if previous.base_sha == current.base_sha {
        return true;
    }
    matches!(
        (merge_base(repo, &previous.base_sha, &previous.head_sha), merge_base(repo, &current.base_sha, &current.head_sha)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// Returns `true` when `current` is a clean forward push on top of `previous` with unchanged base.
pub fn can_review_incrementally(repo: &Path, previous: &ReviewScope, current: &ReviewScope) -> bool {
    if previous.head_sha == current.head_sha || previous.base_ref != current.base_ref {
        return false;
    }
    if !is_ancestor(repo, &previous.head_sha, &current.head_sha) {
        return false;
    }
    matches!(
        (merge_base(repo, &previous.base_sha, &previous.head_sha), merge_base(repo, &current.base_sha, &current.head_sha)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// Excludes lockfiles, generated artifacts, minified bundles, and vendored paths.
pub fn policy_exclusion(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let name = Path::new(&lower).file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(
        name,
        "cargo.lock" | "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock" | "bun.lock" | "bun.lockb" | "go.sum" | "poetry.lock" | "gemfile.lock" | "podfile.lock"
    ) {
        return Some("lockfile");
    }
    if lower.ends_with(".min.js") || lower.ends_with(".min.css") || lower.ends_with(".pb.go") || lower.ends_with(".g.dart") || lower.ends_with(".snap") {
        return Some("generated");
    }
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".ico")
        || lower.ends_with(".pdf")
        || lower.ends_with(".woff2")
    {
        return Some("binary_asset");
    }
    if lower.starts_with("vendor/") || lower.contains("/vendor/") || lower.starts_with("node_modules/") || lower.contains("/node_modules/") {
        return Some("vendored");
    }
    None
}

pub fn changed_files_between(repo: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let raw = git_capture(repo, &["diff", "--name-only", "-z", &format!("{base}..{head}")])?;
    Ok(raw.split('\0').filter(|s| !s.is_empty()).map(str::to_owned).collect())
}

/// Plans the diff scope and reused findings for a review run.
pub fn plan_scope(
    repo: &Path,
    current: &ReviewScope,
    previous: Option<&PrReviewResult>,
    force_full: bool,
    is_stacked_base: bool,
) -> Result<ScopePlan, String> {
    let mb = merge_base(repo, &current.base_sha, &current.head_sha)?;
    let (mode, source_run_id, diff_base_sha, prev_findings) = if !force_full
        && let Some(prev) = previous
        && let Some(prev_run) = &prev.run
    {
        let prev_scope = ReviewScope {
            head_sha: prev_run.head_sha.clone(),
            base_ref: prev_run.base_ref.clone(),
            base_sha: prev_run.base_sha.clone(),
        };
        if can_review_incrementally(repo, &prev_scope, current) {
            ("incremental".to_owned(), Some(prev_run.id.clone()), prev_run.head_sha.clone(), prev.findings.clone())
        } else if prev_scope.base_ref == current.base_ref && prev_scope.base_sha != current.base_sha {
            ("base_sync".to_owned(), Some(prev_run.id.clone()), mb.clone(), Vec::new())
        } else if is_stacked_base {
            ("stacked".to_owned(), None, mb.clone(), Vec::new())
        } else {
            ("full".to_owned(), None, mb.clone(), Vec::new())
        }
    } else if is_stacked_base {
        ("stacked".to_owned(), None, mb.clone(), Vec::new())
    } else {
        ("full".to_owned(), None, mb.clone(), Vec::new())
    };

    let all_changed_files = changed_files_between(repo, &diff_base_sha, &current.head_sha)?;
    let mut reviewable_files = Vec::new();
    let mut excluded_files = Vec::new();
    for path in &all_changed_files {
        if let Some(reason) = policy_exclusion(path) {
            excluded_files.push(ExcludedFile { path: path.clone(), reason });
        } else {
            reviewable_files.push(path.clone());
        }
    }

    // Carry forward prior validated findings for files NOT touched by the new delta.
    let delta_set: HashSet<&str> = all_changed_files.iter().map(String::as_str).collect();
    let reused_findings = if mode == "incremental" {
        prev_findings
            .into_iter()
            .filter(|f| f.path.as_deref().is_none_or(|p| !delta_set.contains(p)))
            .map(|mut f| {
                f.id = crate::util::new_uuid();
                f.source_stage = "reused".into();
                f
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(ScopePlan {
        mode,
        source_run_id,
        merge_base_sha: mb,
        diff_base_sha,
        reviewable_files,
        excluded_files,
        reused_findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut cmd = crate::process::blocking_command("git");
        cmd.current_dir(dir)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .args(args);
        let out = crate::process::output_blocking(&mut cmd, crate::process::QUICK).unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn selects_incremental_scope_and_excludes_lockfiles() {
        let dir = std::env::temp_dir().join(format!("domovoi-scope-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "base"]);
        let base_sha = git(&dir, &["rev-parse", "HEAD"]);

        git(&dir, &["checkout", "-b", "feat"]);
        std::fs::write(dir.join("a.rs"), "fn a() { panic!(); }\n").unwrap();
        std::fs::write(dir.join("Cargo.lock"), "lock\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "c1"]);
        let head1 = git(&dir, &["rev-parse", "HEAD"]);

        let scope1 = ReviewScope { head_sha: head1.clone(), base_ref: "main".into(), base_sha: base_sha.clone() };
        let plan1 = plan_scope(&dir, &scope1, None, false, false).unwrap();
        assert_eq!(plan1.mode, "full");
        assert_eq!(plan1.reviewable_files, vec!["a.rs"]);
        assert_eq!(plan1.excluded_files.len(), 1);
        assert_eq!(plan1.excluded_files[0].reason, "lockfile");

        // Second push touches b.rs only -> incremental reviews only b.rs and reuses finding on a.rs.
        std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "c2"]);
        let head2 = git(&dir, &["rev-parse", "HEAD"]);
        let prev = PrReviewResult {
            run: Some(domovoi_model::typed::PrReviewRun {
                id: "r1".into(),
                head_sha: head1,
                base_ref: "main".into(),
                base_sha: base_sha.clone(),
                ..Default::default()
            }),
            findings: vec![PrReviewFinding {
                id: "f1".into(),
                run_id: "r1".into(),
                kind: "finding".into(),
                severity: "high".into(),
                title: "Panic in a".into(),
                path: Some("a.rs".into()),
                ..Default::default()
            }],
            current_head_sha: None,
        };
        let scope2 = ReviewScope { head_sha: head2, base_ref: "main".into(), base_sha };
        let plan2 = plan_scope(&dir, &scope2, Some(&prev), false, false).unwrap();
        assert_eq!(plan2.mode, "incremental");
        assert_eq!(plan2.reviewable_files, vec!["b.rs"]);
        assert_eq!(plan2.reused_findings.len(), 1);
        assert_eq!(plan2.reused_findings[0].source_stage, "reused");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
