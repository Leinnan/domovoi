//! SQLite persistence for AI pull request review runs and validated findings.

use crate::db::Db;
use domovoi_model::typed::{ModelSelection, PrReviewEvidenceRef, PrReviewFinding, PrReviewResult, PrReviewRiskDriver, PrReviewRun, PrReviewUsage};
use rusqlite::{OptionalExtension, Row, params};

fn read_run(row: &Row<'_>) -> Result<PrReviewRun, rusqlite::Error> {
    let risk_drivers_raw: String = row.get(14)?;
    let model_raw: Option<String> = row.get(16)?;
    let usage_raw: Option<String> = row.get(19)?;
    Ok(PrReviewRun {
        id: row.get(0)?,
        pr_key: row.get(1)?,
        project_id: row.get(2)?,
        host: row.get(3)?,
        repository: row.get(4)?,
        number: row.get::<_, i64>(5)?.max(0) as u64,
        head_sha: row.get(6)?,
        base_ref: row.get(7)?,
        base_sha: row.get(8)?,
        merge_base_sha: row.get(9)?,
        scope_mode: row.get(10)?,
        source_run_id: row.get(11)?,
        status: row.get(12)?,
        phase: row.get(13)?,
        risk_score: row.get::<_, i64>(14 - 1).ok().map_or(0, |v| v.max(0) as u32),
        risk_drivers: serde_json::from_str(&risk_drivers_raw).unwrap_or_default(),
        summary_markdown: row.get(15)?,
        model_selection: model_raw.and_then(|s| serde_json::from_str::<ModelSelection>(&s).ok()),
        published_review_id: row.get(17)?,
        error_detail: row.get(18)?,
        usage: usage_raw.and_then(|s| serde_json::from_str::<PrReviewUsage>(&s).ok()),
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

const RUN_COLS: &str = "id, pr_key, project_id, host, repository, number, head_sha, base_ref, base_sha, merge_base_sha, \
    scope_mode, source_run_id, status, phase, risk_score, risk_drivers_json, summary_markdown, model_selection_json, \
    published_review_id, error_detail, usage_json, created_at, updated_at";

fn read_run_exact(row: &Row<'_>) -> Result<PrReviewRun, rusqlite::Error> {
    let risk_drivers_raw: String = row.get(15)?;
    let model_raw: Option<String> = row.get(17)?;
    let usage_raw: Option<String> = row.get(20)?;
    Ok(PrReviewRun {
        id: row.get(0)?,
        pr_key: row.get(1)?,
        project_id: row.get(2)?,
        host: row.get(3)?,
        repository: row.get(4)?,
        number: row.get::<_, i64>(5)?.max(0) as u64,
        head_sha: row.get(6)?,
        base_ref: row.get(7)?,
        base_sha: row.get(8)?,
        merge_base_sha: row.get(9)?,
        scope_mode: row.get(10)?,
        source_run_id: row.get(11)?,
        status: row.get(12)?,
        phase: row.get(13)?,
        risk_score: row.get::<_, i64>(14)?.max(0) as u32,
        risk_drivers: serde_json::from_str(&risk_drivers_raw).unwrap_or_default(),
        summary_markdown: row.get(16)?,
        model_selection: model_raw.and_then(|s| serde_json::from_str::<ModelSelection>(&s).ok()),
        published_review_id: row.get(18)?,
        error_detail: row.get(19)?,
        usage: usage_raw.and_then(|s| serde_json::from_str::<PrReviewUsage>(&s).ok()),
        created_at: row.get(21)?,
        updated_at: row.get(22)?,
    })
}

fn read_finding(row: &Row<'_>) -> Result<PrReviewFinding, rusqlite::Error> {
    let ev_raw: String = row.get(10)?;
    let cev_raw: String = row.get(11)?;
    Ok(PrReviewFinding {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        severity: row.get(3)?,
        title: row.get(4)?,
        detail: row.get(5)?,
        action: row.get(6)?,
        path: row.get(7)?,
        line: row.get::<_, Option<i64>>(8)?.and_then(|v| (v > 0).then_some(v as u64)),
        side: row.get(9)?,
        evidence: serde_json::from_str::<Vec<PrReviewEvidenceRef>>(&ev_raw).unwrap_or_default(),
        counter_evidence: serde_json::from_str::<Vec<PrReviewEvidenceRef>>(&cev_raw).unwrap_or_default(),
        selected: row.get::<_, i64>(12)? != 0,
        source_stage: row.get(13)?,
    })
}

pub fn insert_run(db: &Db, run: &PrReviewRun) -> Result<(), String> {
    let risk_json = serde_json::to_string(&run.risk_drivers).unwrap_or_else(|_| "[]".into());
    let model_json = run.model_selection.as_ref().and_then(|m| serde_json::to_string(m).ok());
    let usage_json = run.usage.as_ref().and_then(|u| serde_json::to_string(u).ok());
    db.with(|c| {
        c.execute(
            "INSERT INTO pr_review_runs (
                id, pr_key, project_id, host, repository, number, head_sha, base_ref, base_sha, merge_base_sha,
                scope_mode, source_run_id, status, phase, risk_score, risk_drivers_json, summary_markdown,
                model_selection_json, published_review_id, publish_lock_token, error_detail, usage_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, NULL, ?20, ?21, ?22, ?23)",
            params![
                run.id, run.pr_key, run.project_id, run.host, run.repository, run.number as i64,
                run.head_sha, run.base_ref, run.base_sha, run.merge_base_sha,
                run.scope_mode, run.source_run_id, run.status, run.phase, i64::from(run.risk_score),
                risk_json, run.summary_markdown, model_json, run.published_review_id,
                run.error_detail, usage_json, run.created_at, run.updated_at
            ],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| format!("Failed to persist review run: {e}"))
}

pub fn update_progress(db: &Db, run_id: &str, status: &str, phase: &str) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    db.with(|c| {
        c.execute(
            "UPDATE pr_review_runs SET status = ?2, phase = ?3, updated_at = ?4 WHERE id = ?1",
            params![run_id, status, phase, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn update_scope_and_risk(
    db: &Db,
    run_id: &str,
    scope_mode: &str,
    source_run_id: Option<&str>,
    merge_base_sha: &str,
    risk_score: u32,
    risk_drivers: &[PrReviewRiskDriver],
) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    let drivers_json = serde_json::to_string(risk_drivers).unwrap_or_else(|_| "[]".into());
    db.with(|c| {
        c.execute(
            "UPDATE pr_review_runs
             SET scope_mode = ?2, source_run_id = ?3, merge_base_sha = ?4, risk_score = ?5, risk_drivers_json = ?6, updated_at = ?7
             WHERE id = ?1",
            params![run_id, scope_mode, source_run_id, merge_base_sha, i64::from(risk_score), drivers_json, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn save_validated(
    db: &Db,
    run_id: &str,
    summary_markdown: &str,
    model_selection: Option<&ModelSelection>,
    usage: &PrReviewUsage,
    findings: &[PrReviewFinding],
) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    let model_json = model_selection.and_then(|m| serde_json::to_string(m).ok());
    let usage_json = serde_json::to_string(usage).ok();
    db.batch(|| {
        db.with(|c| {
            c.execute("DELETE FROM pr_review_findings WHERE run_id = ?1", params![run_id])?;
            {
                let mut stmt = c.prepare(
                    "INSERT INTO pr_review_findings (
                        id, run_id, kind, severity, title, detail, action, path, line, side,
                        evidence_json, counter_evidence_json, selected, source_stage
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )?;
                for f in findings {
                    let ev = serde_json::to_string(&f.evidence).unwrap_or_else(|_| "[]".into());
                    let cev = serde_json::to_string(&f.counter_evidence).unwrap_or_else(|_| "[]".into());
                    stmt.execute(params![
                        f.id, run_id, f.kind, f.severity, f.title, f.detail, f.action,
                        f.path, f.line.map(|l| l as i64), f.side, ev, cev, i64::from(f.selected), f.source_stage
                    ])?;
                }
            }
            c.execute(
                "UPDATE pr_review_runs
                 SET status = 'validated', phase = 'ready', summary_markdown = ?2, model_selection_json = COALESCE(?3, model_selection_json),
                     usage_json = ?4, error_detail = NULL, updated_at = ?5
                 WHERE id = ?1",
                params![run_id, summary_markdown, model_json, usage_json, now],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
    })
    .map_err(|e| format!("Could not store validated review: {e}"))
}

pub fn mark_failed(db: &Db, run_id: &str, status: &str, detail: &str) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    db.with(|c| {
        c.execute(
            "UPDATE pr_review_runs SET status = ?2, phase = ?2, error_detail = ?3, updated_at = ?4 WHERE id = ?1",
            params![run_id, status, detail, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn set_finding_selected(db: &Db, run_id: &str, finding_id: &str, selected: bool) -> Result<bool, String> {
    let rows = db
        .with(|c| {
            c.execute(
                "UPDATE pr_review_findings SET selected = ?3 WHERE run_id = ?1 AND id = ?2",
                params![run_id, finding_id, i64::from(selected)],
            )
        })
        .map_err(|e| e.to_string())?;
    Ok(rows > 0)
}

pub fn findings_for_run(db: &Db, run_id: &str) -> Result<Vec<PrReviewFinding>, String> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, run_id, kind, severity, title, detail, action, path, line, side,
                    evidence_json, counter_evidence_json, selected, source_stage
             FROM pr_review_findings WHERE run_id = ?1
             ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, path, line, id",
        )?;
        let rows = stmt.query_map(params![run_id], read_finding)?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .map_err(|e| e.to_string())
}

pub fn get_run(db: &Db, run_id: &str) -> Option<PrReviewResult> {
    let run = db
        .read(|c| c.query_row(&format!("SELECT {RUN_COLS} FROM pr_review_runs WHERE id = ?1"), params![run_id], read_run_exact).optional())
        .ok()??;
    let findings = findings_for_run(db, &run.id).unwrap_or_default();
    Some(PrReviewResult { run: Some(run), findings, current_head_sha: None })
}

pub fn latest_for_pr(db: &Db, pr_key: &str, current_head_sha: Option<String>) -> PrReviewResult {
    let run = db
        .read(|c| {
            c.query_row(
                &format!("SELECT {RUN_COLS} FROM pr_review_runs WHERE pr_key = ?1 ORDER BY created_at DESC LIMIT 1"),
                params![pr_key],
                read_run_exact,
            )
            .optional()
        })
        .ok()
        .flatten();
    let findings = run.as_ref().and_then(|r| findings_for_run(db, &r.id).ok()).unwrap_or_default();
    PrReviewResult { run, findings, current_head_sha }
}

pub fn latest_validated_for_pr(db: &Db, pr_key: &str) -> Option<PrReviewResult> {
    let run = db
        .read(|c| {
            c.query_row(
                &format!(
                    "SELECT {RUN_COLS} FROM pr_review_runs
                     WHERE pr_key = ?1 AND status IN ('validated', 'published')
                     ORDER BY created_at DESC LIMIT 1"
                ),
                params![pr_key],
                read_run_exact,
            )
            .optional()
        })
        .ok()??;
    let findings = findings_for_run(db, &run.id).unwrap_or_default();
    Some(PrReviewResult { run: Some(run), findings, current_head_sha: None })
}

pub fn list_runs(db: &Db, pr_key: Option<&str>, project_id: Option<&str>, limit: usize) -> Vec<PrReviewRun> {
    let cap = limit.clamp(1, 100) as i64;
    db.read(|c| match (pr_key, project_id) {
        (Some(pk), _) => {
            let mut stmt = c.prepare(&format!("SELECT {RUN_COLS} FROM pr_review_runs WHERE pr_key = ?1 ORDER BY created_at DESC LIMIT ?2"))?;
            let rows = stmt.query_map(params![pk, cap], read_run_exact)?;
            rows.collect::<Result<Vec<_>, _>>()
        }
        (None, Some(pid)) => {
            let mut stmt = c.prepare(&format!("SELECT {RUN_COLS} FROM pr_review_runs WHERE project_id = ?1 ORDER BY created_at DESC LIMIT ?2"))?;
            let rows = stmt.query_map(params![pid, cap], read_run_exact)?;
            rows.collect::<Result<Vec<_>, _>>()
        }
        (None, None) => {
            let mut stmt = c.prepare(&format!("SELECT {RUN_COLS} FROM pr_review_runs ORDER BY created_at DESC LIMIT ?1"))?;
            let rows = stmt.query_map(params![cap], read_run_exact)?;
            rows.collect::<Result<Vec<_>, _>>()
        }
    })
    .unwrap_or_default()
}

pub fn acquire_publish_lock(db: &Db, run_id: &str, token: &str) -> Result<bool, String> {
    let now = crate::time_fmt::iso_now();
    let changed = db
        .with(|c| {
            c.execute(
                "UPDATE pr_review_runs SET publish_lock_token = ?2, updated_at = ?3
                 WHERE id = ?1 AND publish_lock_token IS NULL AND status = 'validated'",
                params![run_id, token, now],
            )
        })
        .map_err(|e| e.to_string())?;
    Ok(changed > 0)
}

pub fn release_publish_lock(db: &Db, run_id: &str) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    db.with(|c| {
        c.execute("UPDATE pr_review_runs SET publish_lock_token = NULL, updated_at = ?2 WHERE id = ?1", params![run_id, now])?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn mark_published(db: &Db, run_id: &str, review_id: Option<&str>) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    db.with(|c| {
        c.execute(
            "UPDATE pr_review_runs
             SET status = 'published', phase = 'published', published_review_id = ?2, publish_lock_token = NULL, updated_at = ?3
             WHERE id = ?1",
            params![run_id, review_id, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn mark_superseded(db: &Db, run_id: &str, reason: &str) -> Result<(), String> {
    let now = crate::time_fmt::iso_now();
    db.with(|c| {
        c.execute(
            "UPDATE pr_review_runs
             SET status = 'superseded', phase = 'superseded', error_detail = ?2, publish_lock_token = NULL, updated_at = ?3
             WHERE id = ?1",
            params![run_id, reason, now],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| e.to_string())
}

pub fn dangling_publish_locks(db: &Db) -> Vec<PrReviewRun> {
    let _ = read_run; // keep column order verified via `read_run_exact`
    db.read(|c| {
        let mut stmt = c.prepare(&format!("SELECT {RUN_COLS} FROM pr_review_runs WHERE publish_lock_token IS NOT NULL"))?;
        let rows = stmt.query_map([], read_run_exact)?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_default()
}
