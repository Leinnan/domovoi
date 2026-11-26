//! Idempotent, live-head-verified publication of validated AI code reviews to
//! GitHub (`gh`) and GitLab (`glab`).
//!
//! Guarantees:
//! - Never auto-approves a pull request (always submits non-blocking `comment` verdict).
//! - Refuses to publish if live PR state/head/scope became stale.
//! - Uses a SQLite `publish_lock_token` + HTML marker (`<!-- domovoi-review:v1 ... -->`)
//!   for crash-safe idempotency and ambiguous-POST reconciliation.
//! - Splits large inline finding sets into batches of 25 comments with retry backoff
//!   on rate limits.

use crate::db::Db;
use crate::gh::GhReason;
use crate::review::published_text::sanitize_markdown;
use crate::review::scope::{self, ReviewScope};
use crate::review::store;
use crate::gh::lenient::{Fields, Loose, Scalar};
use crate::rpc::source_control::github;
use crate::rpc::source_control::gitlab;
use crate::rpc::source_control::pull_request_failure::Provider;
use crate::rpc::source_control::pull_requests::{ReviewCommentDraft, ReviewPosition, Supported};
use domovoi_model::typed::{PrReviewFinding, PrReviewRun};
use std::path::Path;
use std::time::Duration;

const BATCH_SIZE: usize = 25;
const RATE_LIMIT_WAITS_SECS: [u64; 3] = [5, 15, 30];

pub fn review_marker(repository: &str, number: u64, run_id: &str, head_sha: &str) -> String {
    format!("<!-- domovoi-review:v1 repo={repository} pr={number} run={run_id} head={head_sha} -->")
}

/// Renders the structured summary Markdown posted with the review.
pub fn render_summary_body(run: &PrReviewRun, findings: &[PrReviewFinding], marker: &str) -> String {
    let selected_findings: Vec<&PrReviewFinding> = findings.iter().filter(|f| f.kind == "finding" && f.selected).collect();
    let suggestions: Vec<&PrReviewFinding> = findings.iter().filter(|f| f.kind == "suggestion" && f.selected).collect();
    let questions: Vec<&PrReviewFinding> = findings.iter().filter(|f| f.kind == "question" && f.selected).collect();

    let mut out = String::new();
    out.push_str(marker);
    out.push_str("\n\n");
    out.push_str(&format!(
        "### Domovoi AI Code Review (`{}` · risk {}/100)\n\n",
        run.scope_mode, run.risk_score
    ));
    if let Some(summary) = &run.summary_markdown
        && !summary.trim().is_empty()
    {
        out.push_str(&sanitize_markdown(summary.trim()));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "- **Reviewed head:** `{}` (scope `{}`)\n- **Inline findings posted:** {}\n",
        &run.head_sha[..run.head_sha.len().min(12)],
        run.scope_mode,
        selected_findings.len()
    ));
    if !run.risk_drivers.is_empty() {
        out.push_str("\n<details>\n<summary>Risk breakdown</summary>\n\n");
        for d in &run.risk_drivers {
            out.push_str(&format!("- **{}** (+{}): {}\n", d.label, d.points, d.detail));
        }
        out.push_str("\n</details>\n");
    }
    if !suggestions.is_empty() {
        out.push_str("\n<details>\n<summary>Suggestions</summary>\n\n");
        for s in suggestions {
            let loc = s.path.as_deref().map_or(String::new(), |p| format!(" (`{p}:{}`)", s.line.unwrap_or(1)));
            out.push_str(&format!("- **{}**{loc}: {} — {}\n", s.title, s.detail, s.action));
        }
        out.push_str("\n</details>\n");
    }
    if !questions.is_empty() {
        out.push_str("\n<details>\n<summary>Questions</summary>\n\n");
        for q in questions {
            out.push_str(&format!("- **{}**: {}\n", q.title, q.detail));
        }
        out.push_str("\n</details>\n");
    }
    out
}

fn format_inline_comment(f: &PrReviewFinding) -> String {
    let badge = match f.severity.as_str() {
        "high" => "🔴 **High**",
        "low" => "🔵 **Low**",
        _ => "🟡 **Medium**",
    };
    let mut body = format!("{badge} — **{}**\n\n{}", f.title, f.detail);
    if !f.action.trim().is_empty() {
        body.push_str(&format!("\n\n**Suggested action:** {}", f.action));
    }
    if let Some(ev) = f.evidence.first() {
        body.push_str(&format!("\n\n> Evidence (`{}:{}`): `{}`", ev.path, ev.line, ev.quote));
    }
    body
}

fn to_drafts(findings: &[PrReviewFinding]) -> Vec<ReviewCommentDraft> {
    findings
        .iter()
        .filter(|f| f.kind == "finding" && f.selected)
        .filter_map(|f| {
            let path = f.path.clone()?;
            let line = f.line.filter(|l| *l > 0)?;
            let is_old = f.side.as_deref() == Some("old");
            let position = if is_old {
                ReviewPosition {
                    kind: Loose(Some("deleted".into())),
                    new_line: Loose(None),
                    old_line: Loose(Some(line)),
                    side: Loose(Some("left".into())),
                }
            } else {
                ReviewPosition {
                    kind: Loose(Some("added".into())),
                    new_line: Loose(Some(line)),
                    old_line: Loose(None),
                    side: Loose(Some("right".into())),
                }
            };
            Some(ReviewCommentDraft {
                path: Loose(Some(Scalar::Text(path))),
                old_path: Loose(None),
                body: Loose(Some(Scalar::Text(format_inline_comment(f)))),
                position: Fields(position),
                start_position: Fields(ReviewPosition::default()),
            })
        })
        .collect()
}

/// Checks whether the live PR is still open and matches the reviewed head & scope.
async fn verify_live_scope(project: &Supported, run: &PrReviewRun) -> Result<(), String> {
    let live_head = match project.provider {
        Provider::GitHub => github::head_sha(project, run.number).await.map_err(|e| e.detail)?,
        Provider::GitLab => gitlab::head_sha(project, run.number).await.map_err(|e| e.detail)?,
    };
    let Some(live_head) = live_head else {
        return Err("Could not read live pull request head SHA before publishing.".into());
    };
    if live_head != run.head_sha {
        return Err(format!(
            "Pull request head moved from {} to {}; refresh the review before publishing.",
            &run.head_sha[..run.head_sha.len().min(8)],
            &live_head[..live_head.len().min(8)]
        ));
    }
    let repo_path = Path::new(&project.workspace_root);
    let expected = ReviewScope {
        head_sha: run.head_sha.clone(),
        base_ref: run.base_ref.clone(),
        base_sha: run.base_sha.clone(),
    };
    let current = ReviewScope {
        head_sha: live_head,
        base_ref: run.base_ref.clone(),
        base_sha: run.base_sha.clone(),
    };
    if !scope::equivalent(repo_path, &expected, &current) {
        return Err("Pull request base/diff scope changed; run a fresh review before publishing.".into());
    }
    Ok(())
}

/// Checks if our HTML marker was already posted to the PR (idempotent reconciliation).
async fn marker_already_posted(project: &Supported, number: u64, marker: &str) -> Result<bool, String> {
    let activity = match project.provider {
        Provider::GitHub => github::activity(project, number).await.map_err(|e| e.detail)?,
        Provider::GitLab => gitlab::activity(project, number).await.map_err(|e| e.detail)?,
    };
    let Some(activity) = activity else { return Ok(false) };
    let found = activity.comments.iter().any(|c| c.body.contains(marker))
        || activity.review_threads.iter().any(|t| t.comments.iter().any(|c| c.body.contains(marker)));
    Ok(found)
}

async fn submit_batch_with_retry(project: &Supported, number: u64, body: &str, comments: &[ReviewCommentDraft]) -> Result<(), String> {
    let mut attempt = 0usize;
    loop {
        let res = match project.provider {
            Provider::GitHub => github::submit_review(project, number, "comment", body, comments).await,
            Provider::GitLab => gitlab::submit_review(project, number, "comment", body, comments).await,
        };
        match res {
            Ok(()) => return Ok(()),
            Err(err) if err.reason == GhReason::RateLimited && attempt < RATE_LIMIT_WAITS_SECS.len() => {
                tokio::time::sleep(Duration::from_secs(RATE_LIMIT_WAITS_SECS[attempt])).await;
                attempt += 1;
            }
            Err(err) => return Err(err.detail),
        }
    }
}

/// Publishes the selected validated findings of `run_id` as a non-blocking review.
pub async fn publish_run(db: &Db, project: &Supported, run_id: &str) -> Result<PrReviewRun, String> {
    let result = store::get_run(db, run_id).ok_or_else(|| "Review run not found.".to_owned())?;
    let run = result.run.ok_or_else(|| "Review run not found.".to_owned())?;
    if run.status == "published" {
        return Ok(run);
    }
    if run.status != "validated" {
        return Err(format!("Only validated reviews can be published (current status: {}).", run.status));
    }

    verify_live_scope(project, &run).await?;

    let token = crate::util::new_uuid();
    if !store::acquire_publish_lock(db, run_id, &token)? {
        return Err("Review publication is already in progress.".into());
    }

    let marker = review_marker(&run.repository, run.number, &run.id, &run.head_sha);
    let summary_body = render_summary_body(&run, &result.findings, &marker);
    let drafts = to_drafts(&result.findings);

    let post_outcome: Result<(), String> = async {
        if drafts.is_empty() {
            submit_batch_with_retry(project, run.number, &summary_body, &[]).await?;
        } else {
            let chunks: Vec<&[ReviewCommentDraft]> = drafts.chunks(BATCH_SIZE).collect();
            let total = chunks.len();
            for (idx, chunk) in chunks.iter().enumerate() {
                if idx > 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let is_last = idx + 1 == total;
                let body = if is_last {
                    summary_body.clone()
                } else {
                    format!(
                        "{marker}\n\nDomovoi AI Code Review inline comments, part {} of {total}. The summary is in part {total}.",
                        idx + 1
                    )
                };
                submit_batch_with_retry(project, run.number, &body, chunk).await?;
            }
        }
        Ok(())
    }
    .await;

    match post_outcome {
        Ok(()) => {
            store::mark_published(db, run_id, Some(&marker))?;
            // Post-publish live head check: detect a race push.
            if let Err(stale) = verify_live_scope(project, &run).await {
                let _ = store::mark_superseded(db, run_id, &stale);
            }
            store::get_run(db, run_id).and_then(|r| r.run).ok_or_else(|| "Published run disappeared.".into())
        }
        Err(post_err) => match marker_already_posted(project, run.number, &marker).await {
            Ok(true) => {
                store::mark_published(db, run_id, Some(&marker))?;
                store::get_run(db, run_id).and_then(|r| r.run).ok_or_else(|| "Published run disappeared.".into())
            }
            Ok(false) => {
                let _ = store::release_publish_lock(db, run_id);
                Err(post_err)
            }
            Err(reconcile_err) => Err(format!(
                "{post_err}; host acceptance is ambiguous and publish lock was retained for reconciliation: {reconcile_err}"
            )),
        },
    }
}

/// Startup helper: releases or reconciles any publish locks left behind by a crash.
pub fn log_dangling_locks(db: &Db) {
    let locked = store::dangling_publish_locks(db);
    if !locked.is_empty() {
        tracing::warn!(count = locked.len(), "found PR review runs with dangling publish locks from a prior run");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::{PrReviewEvidenceRef, PrReviewRiskDriver};

    #[test]
    fn renders_marker_and_respects_selected_triage_flags() {
        let run = PrReviewRun {
            id: "run-1".into(),
            repository: "acme/app".into(),
            number: 42,
            head_sha: "abcdef1234567890".into(),
            scope_mode: "incremental".into(),
            risk_score: 55,
            risk_drivers: vec![PrReviewRiskDriver { points: 20, label: "Blast radius".into(), detail: "3 packages".into() }],
            summary_markdown: Some("Looks solid except one panic.".into()),
            ..Default::default()
        };
        let findings = vec![
            PrReviewFinding {
                id: "f1".into(),
                run_id: "run-1".into(),
                kind: "finding".into(),
                severity: "high".into(),
                title: "Panic on empty input".into(),
                detail: "Calls unwrap".into(),
                action: "Return Result".into(),
                path: Some("src/lib.rs".into()),
                line: Some(12),
                side: Some("new".into()),
                evidence: vec![PrReviewEvidenceRef { path: "src/lib.rs".into(), line: 12, quote: "x.unwrap()".into() }],
                counter_evidence: vec![],
                selected: true,
                source_stage: "primary".into(),
            },
            PrReviewFinding {
                id: "f2".into(),
                run_id: "run-1".into(),
                kind: "finding".into(),
                severity: "low".into(),
                title: "Dismissed finding".into(),
                path: Some("src/lib.rs".into()),
                line: Some(20),
                side: Some("new".into()),
                selected: false,
                source_stage: "lens".into(),
                ..Default::default()
            },
        ];
        let marker = review_marker("acme/app", 42, "run-1", "abcdef1234567890");
        let body = render_summary_body(&run, &findings, &marker);
        assert!(body.contains(&marker));
        assert!(body.contains("Inline findings posted:** 1"));
        let drafts = to_drafts(&findings);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].position.new_line.0, Some(12));
    }
}
