//! `server.*` administration RPCs: config, settings, keybindings, storage
//! and database maintenance, diagnostics, resource history, usage, secret
//! and endpoint checks, triage prompts.
//!
//! Limits, each answered with t3code's typed error or empty state:
//! - self-update, desktop updates and npm releases: Domovoi is not distributed
//!   through npm or the desktop app;
//! - provider CLI updates (`server.updateProvider`, `crate::provider_update`):
//!   npm, Homebrew and the CLIs' own updaters, without version advisories;
//! - the ACP registry: agents are searched, installed and removed
//!   (`crate::acp_registry`) and their URL sign-ins accepted
//!   (`crate::adapters::acp::url_auth`), but an agent's own sessions, model
//!   providers and sign-out are not driven from here.
//!
//! Trace diagnostics read the trace file `crate::trace_writer` writes
//! (`crate::trace_diagnostics`); usage is priced from LiteLLM's rate table
//! (`crate::usage_pricing`).

#![expect(clippy::disallowed_types, reason = "the model list a text generator's endpoint answers; request fields read leniently inside their readers")]

use super::Never;
use crate::config_failure::Failure as ConfigFailure;
use crate::keybindings::{KeybindingRule, UpsertInput};
use super::admin_failure::Failure;
use super::telemetry::Signal;
use crate::state::ConnCtx;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use domovoi_model::typed::{
    AppThread, DatabaseStorageStatus, Run, RunStatus, SettledOverride, StorageCleanupCounts, StorageCleanupKind, StorageCleanupOutcome, StorageCleanupReport,
    StorageCleanupReportEntry, StorageCleanupTrigger, Timestamp,
};
use tokio::sync::broadcast;

/// What `server.updateSettings` is asked: a patch of the settings, and
/// optionally one provider instance to create, replace or remove.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettingsInput {
    #[serde(default)]
    patch: crate::settings::SettingsPatch,
    #[serde(default, deserialize_with = "crate::settings::InstanceMutation::optional")]
    provider_instance_mutation: Option<crate::settings::InstanceMutation>,
}

/// `server.updateSettings`: the settings after the change, once they are saved.
async fn update_settings(ctx: Arc<ConnCtx>, input: UpdateSettingsInput) -> Result<crate::settings::Settings, ConfigFailure> {
    let mut settings = ctx.state.settings.write().await;
    let mut next = settings.patched(&input.patch);
    if let Some(mutation) = &input.provider_instance_mutation {
        next = next.with_instance(mutation);
    }
    // The managed policy vets the write before anything is stored (`vetWrite`).
    let policy = crate::policy::current();
    if policy.is_managed() {
        let patch = crate::settings::Settings::patch_fields(&input.patch);
        if let Err(refusal) = policy.vet_write(&settings.document_fields(), &next.document_fields(), patch) {
            return Err(ConfigFailure::managed(&crate::settings::path(&ctx.state.paths.userdata), refusal));
        }
        next = next.with_locks();
    }
    // Credentials go to the secret store; the settings keep a mark in their place.
    let next = crate::secrets::stash(&ctx.state.paths.userdata, next);
    let launches = crate::antigravity::launch_keys();
    crate::settings::save(&ctx.state.paths.userdata, &next)?;
    *settings = next.clone();
    drop(settings);
    // Sessions started with an instance's old sign-in settings stop now (t3code's do).
    crate::antigravity::stop_changed(&launches).await;
    Ok(next)
}

/// `AcpRegistrySearchInput`.
#[derive(serde::Deserialize)]
struct AcpSearchInput {
    #[serde(default)]
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// `AcpRegistrySearchResult`.
#[derive(serde::Serialize)]
struct AcpSearchResult {
    agents: Vec<crate::acp_registry::Listed>,
}

/// `AcpRegistryPrepareInput`, `AcpRegistryManagedBinaryUninstallInput`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcpAgentInput {
    agent_id: String,
}

// ---- reading what is sent ----

/// Reads a field as these handlers always have: its value when it is of the
/// type asked for, nothing when it is left out or of another type.
fn lenient<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(sent: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(sent)?).ok())
}

/// The record of an object whose fields are all read leniently; nothing for anything else.
fn fields<T: DeserializeOwned>(sent: Value) -> Option<T> {
    sent.is_object().then(|| serde_json::from_value(sent).ok()).flatten()
}

/// Reads a record of lenient fields: every default when what was sent is no object.
fn fields_or_default<'de, D: serde::Deserializer<'de>, T: DeserializeOwned + Default>(sent: D) -> Result<T, D::Error> {
    Ok(fields(Value::deserialize(sent)?).unwrap_or_default())
}

/// Reads a record of lenient fields that may be left out: nothing when what was sent is no object.
fn fields_if_object<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(sent: D) -> Result<Option<T>, D::Error> {
    Ok(fields(Value::deserialize(sent)?))
}

/// The payload of a call whose every field is read leniently. A payload that
/// is no object at all reads as one with nothing in it.
struct Sent<T>(T);

impl<'de, T: DeserializeOwned + Default> Deserialize<'de> for Sent<T> {
    fn deserialize<D: serde::Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        fields_or_default(sent).map(Sent)
    }
}

// ---- storage cleanup ----

const LOG_RETENTION_DAYS: u64 = 14;

fn cleanup_reports() -> &'static (Mutex<Option<StorageCleanupReport>>, broadcast::Sender<StorageCleanupReport>) {
    static R: OnceLock<(Mutex<Option<StorageCleanupReport>>, broadcast::Sender<StorageCleanupReport>)> = OnceLock::new();
    R.get_or_init(|| (Mutex::new(None), broadcast::channel(8).0))
}

/// Which local files keep a worktree from being removed (`WorktreeKeepWhen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum KeepWhen {
    TrackedChanges,
    #[default]
    UncommittedChanges,
    AnyLocalFiles,
}

impl KeepWhen {
    /// Anything but the two other spellings is the default.
    fn read<'de, D: serde::Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        Ok(match lenient::<D, String>(sent)?.as_deref() {
            Some("tracked-changes") => KeepWhen::TrackedChanges,
            Some("any-local-files") => KeepWhen::AnyLocalFiles,
            _ => KeepWhen::UncommittedChanges,
        })
    }
}

/// The storage cleanup rules as a cleanup reads them from the settings
/// (`StorageCleanupSettings`): a rule left out, or not of its type, is not set.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Rules {
    #[serde(deserialize_with = "lenient")]
    logs_after_days: Option<u64>,
    /// `None` when unset (`null` or left out), `Some(None)` when set to what is no count of days.
    #[serde(deserialize_with = "Rules::days")]
    worktree_after_days: Option<Option<i64>>,
    #[serde(deserialize_with = "lenient")]
    worktree_settled_after_days: Option<i64>,
    #[serde(deserialize_with = "lenient")]
    worktree_on_delete: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    worktree_unchanged: Option<bool>,
    #[serde(rename = "worktreeKeepWhen", deserialize_with = "KeepWhen::read")]
    keep_when: KeepWhen,
}

impl Rules {
    /// The rules a `storageCleanup` setting holds.
    #[cfg(test)]
    fn of(storage_cleanup: &Value) -> Self {
        fields(storage_cleanup.clone()).unwrap_or_default()
    }

    fn days<'de, D: serde::Deserializer<'de>>(sent: D) -> Result<Option<Option<i64>>, D::Error> {
        let sent = Value::deserialize(sent)?;
        Ok((!sent.is_null()).then(|| sent.as_i64()))
    }
}

/// What a cleanup did, before it is counted and timed.
struct Cleaned {
    started_at: String,
    entries: Vec<StorageCleanupReportEntry>,
    bytes_freed: u64,
}

/// A report entry about `path` with nothing said of a thread or a size.
fn entry(kind: StorageCleanupKind, outcome: StorageCleanupOutcome, reason: impl Into<String>, path: String) -> StorageCleanupReportEntry {
    StorageCleanupReportEntry { kind, outcome, reason: reason.into(), path: Some(path), project_path: None, thread_id: None, thread_title: None, bytes: None, files: None }
}

/// Removes old log files, then thread worktrees the `storageCleanup`
/// rules allow (`worktree_entries`); publishes the report.
async fn run_cleanup_all(state: &crate::state::AppState, trigger: StorageCleanupTrigger) -> StorageCleanupReport {
    let logs = state.paths.logs.clone();
    let worktrees = state.paths.home.join("worktrees");
    // Read leniently, rule by rule: one rule of another type does not unset the others.
    let rules: Rules = state.settings.read().await.view_at(&["storageCleanup"]).unwrap_or_default();
    let days = rules.logs_after_days.unwrap_or(LOG_RETENTION_DAYS);
    let mut cleaned = tokio::task::spawn_blocking(move || run_cleanup_logs(&logs, days))
        .await
        // The log sweep panicked: the report is of the worktrees alone.
        .unwrap_or_else(|_| Cleaned { started_at: domovoi_contracts::iso_now(), entries: vec![], bytes_freed: 0 });
    let (wt_entries, freed) = worktree_entries(&state.store, &rules, &worktrees).await;
    cleaned.entries.extend(wt_entries);
    cleaned.bytes_freed += freed;
    publish(trigger, cleaned)
}

/// Total size of the files under `dir` (a worktree's `.git` file included).
fn dir_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() { stack.push(e.path()) } else { total += m.len() }
        }
    }
    total
}

async fn git_out(cwd: &str, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process::git(cwd);
    cmd.args(args);
    let o = crate::process::output(cmd, crate::process::SLOW).await.ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Why a thread's worktree must stay, by t3code's `storageCleanup` rules:
/// - eligible only when deleted (`worktreeOnDelete`), inactive for
///   `worktreeAfterDays`, settled for `worktreeSettledAfterDays`, or
///   settled/archived and unchanged (`worktreeUnchanged`);
/// - never with an agent at work, local files `worktreeKeepWhen` keeps, or
///   commits that are neither pushed nor in the project checkout.
/// `None` means it can go.
async fn worktree_keep_reason(thread: &AppThread, active: bool, root: &str, path: &str, rules: &Rules, now_ms: i64) -> Option<String> {
    const DAY: i64 = 86_400_000;
    if active {
        return Some("An agent is working in it.".into());
    }
    let ms = |at: &Timestamp| domovoi_contracts::time_fmt::millis_from_iso(at.as_str());
    let deleted = thread.deleted_at.is_some();
    let settled = thread.settled_override == Some(Some(SettledOverride::Settled));
    let settled_at = settled.then(|| thread.settled_at.as_ref().and_then(Option::as_ref).and_then(ms)).flatten();
    let archived = thread.archived_at.is_some();
    let activity = ms(&thread.updated_at).unwrap_or(now_ms);
    let after_days = rules.worktree_after_days.flatten();
    let on_delete = rules.worktree_on_delete == Some(true);
    let unchanged_rule = rules.worktree_unchanged == Some(true);
    let old = after_days.is_some_and(|d| activity < now_ms - d * DAY);
    let settled_days = settled_at.map(|s| (now_ms - s.max(activity)).max(0) / DAY);
    let settled = match (settled_days, rules.worktree_settled_after_days) {
        (Some(have), Some(need)) => have >= need,
        _ => false,
    };
    let tracked = git_out(path, &["status", "--porcelain", "--untracked-files=no"]).await;
    let untracked = git_out(path, &["status", "--porcelain"]).await;
    let ignored = git_out(path, &["status", "--porcelain", "--ignored"]).await;
    let (Some(tracked), Some(untracked), Some(ignored)) = (tracked, untracked, ignored) else {
        return Some("The worktree could not be inspected.".into());
    };
    let main_head = git_out(root, &["rev-parse", "HEAD"]).await.map(|h| h.trim().to_owned()).unwrap_or_default();
    let mut unpushed_args = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    if !main_head.is_empty() {
        unpushed_args.push(&main_head);
    }
    let unpushed = git_out(path, &unpushed_args).await.and_then(|c| c.trim().parse::<u64>().ok()).unwrap_or(1);
    let unchanged = untracked.trim().is_empty() && unpushed == 0;
    let eligible = (deleted && on_delete) || old || settled || (unchanged_rule && (settled_at.is_some() || archived) && unchanged);
    if !eligible {
        return Some(match (settled_days, rules.worktree_settled_after_days) {
            (_, None) if rules.worktree_after_days.is_none() && !on_delete && !unchanged_rule => "No storage cleanup rule removes worktrees.".into(),
            (None, Some(_)) => "Not settled.".into(),
            (Some(have), Some(need)) => format!("Settled for {have} of {need} days."),
            _ => "No storage cleanup rule applies to it yet.".into(),
        });
    }
    let local = match rules.keep_when {
        KeepWhen::TrackedChanges => !tracked.trim().is_empty(),
        KeepWhen::AnyLocalFiles => !ignored.trim().is_empty(),
        KeepWhen::UncommittedChanges => !untracked.trim().is_empty(),
    };
    if local {
        return Some("It has local changes.".into());
    }
    if unpushed > 0 {
        return Some("It has commits that are not pushed or in the project checkout.".into());
    }
    None
}

/// Report entries for thread worktrees (and stray folders in Domovoi's own
/// worktrees directory, which are kept), removing those the rules allow.
async fn worktree_entries(store: &crate::store::Store, rules: &Rules, worktrees_dir: &std::path::Path) -> (Vec<StorageCleanupReportEntry>, u64) {
    let now_ms = domovoi_contracts::time_fmt::now_millis();
    let mut entries = vec![];
    let mut freed = 0;
    let mut seen = std::collections::HashSet::new();
    let mut threads = store.app_threads().unwrap_or_default();
    // The newest thread decides for a worktree several threads used.
    threads.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
    for t in &threads {
        let Some(path) = t.worktree_path.as_deref().filter(|p| std::path::Path::new(p).join(".git").is_file()) else { continue };
        if !seen.insert(path.to_owned()) {
            continue;
        }
        let Ok(Some(project)) = store.get_project(t.project_id.as_str()) else { continue };
        let tid = t.id.as_str();
        let active = store.records::<Run>(tid).is_ok_and(|runs| {
            runs.iter().any(|run| run.status == RunStatus::Queued || crate::orchestration::commands::ACTIVE_RUN.contains(&run.status.as_str()))
        });
        let bytes = { let p = std::path::PathBuf::from(path); tokio::task::spawn_blocking(move || dir_bytes(&p)).await.unwrap_or(0) };
        let (outcome, reason) = match worktree_keep_reason(t, active, &project.workspace_root, path, rules, now_ms).await {
            Some(reason) => (StorageCleanupOutcome::Kept, reason),
            None => {
                // Disposable untracked files only remain under `tracked-changes`.
                let force = rules.keep_when == KeepWhen::TrackedChanges;
                match crate::vcs_ops::remove_worktree(&project.workspace_root, path, force).await {
                    Ok(_) => {
                        freed += bytes;
                        (StorageCleanupOutcome::Removed, "Its thread is done and it has nothing unsaved.".to_owned())
                    }
                    Err(e) => (StorageCleanupOutcome::Failed, e.detail),
                }
            }
        };
        entries.push(StorageCleanupReportEntry {
            thread_id: Some(t.id.clone()),
            thread_title: Some(t.title.clone()),
            bytes: Some(Some(bytes.into())),
            ..entry(StorageCleanupKind::Worktree, outcome, reason, path.to_owned())
        });
    }
    for repo in std::fs::read_dir(worktrees_dir).into_iter().flatten().flatten() {
        for wt in std::fs::read_dir(repo.path()).into_iter().flatten().flatten() {
            let path = wt.path().to_string_lossy().into_owned();
            if !seen.contains(&path) {
                entries.push(entry(StorageCleanupKind::Worktree, StorageCleanupOutcome::Kept, "No thread on this server uses it.", path));
            }
        }
    }
    (entries, freed)
}

/// Removes log files older than `days`.
fn run_cleanup_logs(logs: &std::path::Path, days: u64) -> Cleaned {
    let started_at = domovoi_contracts::iso_now();
    let mut entries = vec![];
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let mut freed = 0u64;
    for e in std::fs::read_dir(logs).into_iter().flatten().flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() || meta.modified().map(|m| m > cutoff).unwrap_or(true) {
            continue;
        }
        let path = e.path().to_string_lossy().into_owned();
        let (outcome, reason) = match std::fs::remove_file(e.path()) {
            Ok(()) => {
                freed += meta.len();
                (StorageCleanupOutcome::Removed, format!("Older than {days} days."))
            }
            Err(err) => (StorageCleanupOutcome::Failed, err.to_string()),
        };
        entries.push(StorageCleanupReportEntry { bytes: Some(Some(meta.len().into())), files: Some(Some(1.into())), ..entry(StorageCleanupKind::Logs, outcome, reason, path) });
    }
    Cleaned { started_at, entries, bytes_freed: freed }
}

/// Counts and times the report, keeps it as the latest and sends it.
fn publish(trigger: StorageCleanupTrigger, cleaned: Cleaned) -> StorageCleanupReport {
    let Cleaned { started_at, entries, bytes_freed } = cleaned;
    let count = |outcome: StorageCleanupOutcome| Number::from(entries.iter().filter(|e| e.outcome == outcome).count());
    let counts = StorageCleanupCounts {
        removed: count(StorageCleanupOutcome::Removed),
        kept: count(StorageCleanupOutcome::Kept),
        failed: count(StorageCleanupOutcome::Failed),
    };
    let report = StorageCleanupReport {
        trigger,
        started_at: started_at.into(),
        finished_at: domovoi_contracts::iso_now().into(),
        entries,
        counts,
        omitted_count: 0.into(),
        bytes_freed: Some(bytes_freed.into()),
    };
    let (last, tx) = cleanup_reports();
    *crate::util::locked(&last) = Some(report.clone());
    let _ = tx.send(report.clone());
    report
}

fn cleanup_report_stream() -> BoxStream<'static, Result<StorageCleanupReport, Never>> {
    let (last, tx) = cleanup_reports();
    let mut rx = tx.subscribe();
    let first = crate::util::locked(&last).clone();
    async_stream::stream! {
        if let Some(r) = first { yield Ok(r); }
        while let Ok(r) = rx.recv().await { yield Ok(r); }
    }
    .boxed()
}

// ---- database ----

fn database_storage(ctx: &ConnCtx) -> DatabaseStorageStatus {
    let db = ctx.state.store.db();
    let pragma = |name: &str| db.with(|c| c.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0))).unwrap_or(0);
    let page_size = pragma("page_size");
    let path = ctx.state.paths.userdata.join("state.sqlite");
    DatabaseStorageStatus {
        database_bytes: (pragma("page_count") * page_size).into(),
        reclaimable_bytes: (pragma("freelist_count") * page_size).into(),
        incremental_vacuum_enabled: pragma("auto_vacuum") == 2,
        compact_command: format!("sqlite3 '{}' 'VACUUM;'", path.display()),
        can_reclaim: Some(true),
        reclaim_failure: None,
    }
}

fn database_stream(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<DatabaseStorageStatus, Never>> {
    async_stream::stream! {
        let mut last = None;
        loop {
            let now = database_storage(&ctx);
            if last.as_ref() != Some(&now) {
                yield Ok(now.clone());
                last = Some(now);
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    }
    .boxed()
}

// ---- checks ----

/// Runs curl: `Ok((stdout, elapsed))`, or `Err(exit code)` (28 is a timeout).
async fn curl(args: &[&str]) -> Result<(String, std::time::Duration), i32> {
    let started = std::time::Instant::now();
    let out = crate::process::command("curl").args(args).kill_on_drop(true).output().await.map_err(|_| -1)?;
    if out.status.success() { Ok((String::from_utf8_lossy(&out.stdout).into_owned(), started.elapsed())) } else { Err(out.status.code().unwrap_or(-1)) }
}

/// What an OTLP endpoint is asked to take (`OtlpSignal`).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OtlpSignal {
    Traces,
    Metrics,
    Logs,
}

/// What `server.checkOtlpEndpoint` is asked with. A signal left out or unknown is traces.
#[derive(Default, Deserialize)]
#[serde(default)]
struct OtlpCheckInput {
    #[serde(deserialize_with = "lenient")]
    url: Option<String>,
    #[serde(deserialize_with = "lenient")]
    signal: Option<OtlpSignal>,
}

/// How an OTLP endpoint took an empty export (`OtlpEndpointCheckResult`).
#[derive(Debug, Serialize)]
#[serde(tag = "_tag", rename_all_fields = "camelCase")]
enum OtlpCheck {
    Accepted { latency_ms: f64 },
    /// It answered, with a status that is no success.
    Rejected { status: u16 },
    Unreachable { timed_out: bool },
}

/// `server.checkOtlpEndpoint`: an empty OTLP/JSON export to the endpoint.
async fn check_otlp(input: OtlpCheckInput) -> OtlpCheck {
    let url = input.url.as_deref().unwrap_or_default();
    let body = match input.signal {
        Some(OtlpSignal::Metrics) => r#"{"resourceMetrics":[]}"#,
        Some(OtlpSignal::Logs) => r#"{"resourceLogs":[]}"#,
        _ => r#"{"resourceSpans":[]}"#,
    };
    let args = ["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "5", "-X", "POST", "-H", "content-type: application/json", "--data", body, url];
    match curl(&args).await {
        Ok((code, elapsed)) => match code.trim().parse::<u16>() {
            Ok(c) if (200..300).contains(&c) => OtlpCheck::Accepted { latency_ms: elapsed.as_secs_f64() * 1000.0 },
            Ok(c) if c > 0 => OtlpCheck::Rejected { status: c },
            _ => OtlpCheck::Unreachable { timed_out: false },
        },
        Err(code) => OtlpCheck::Unreachable { timed_out: code == 28 },
    }
}

/// What this server reads of a text generator's settings (`TextGeneratorConfig`).
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TextGenerator {
    #[serde(deserialize_with = "lenient")]
    base_url: Option<String>,
    /// The key itself; a reference to a secret is not one.
    #[serde(deserialize_with = "lenient")]
    api_key: Option<String>,
    #[serde(deserialize_with = "lenient")]
    kind: Option<String>,
}

/// What `server.listTextGeneratorModels` is asked with (`ListTextGeneratorModelsInput`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct ListModelsInput {
    #[serde(deserialize_with = "lenient")]
    id: Option<String>,
    /// Unsaved settings to list with, in place of the ones saved under `id`.
    #[serde(deserialize_with = "fields_if_object")]
    config: Option<TextGenerator>,
    #[serde(deserialize_with = "lenient")]
    test: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    model: Option<String>,
}

/// A model a text generator offers (`TextGeneratorModel`).
#[derive(Debug, Serialize)]
struct TextGeneratorModel {
    id: String,
    name: String,
}

/// `server.listTextGeneratorModels` (`ListTextGeneratorModelsResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextGeneratorModels {
    models: Vec<TextGeneratorModel>,
    /// With `test`, the model the call named (`null` when it named none): Domovoi
    /// lists the models without trying one.
    #[serde(skip_serializing_if = "Option::is_none")]
    tested_model: Option<Option<String>>,
}

/// `server.listTextGeneratorModels`: the model list of an OpenAI-compatible or Anthropic endpoint.
async fn list_text_generator_models(ctx: Arc<ConnCtx>, Sent(input): Sent<ListModelsInput>) -> Result<TextGeneratorModels, Failure> {
    let id = input.id.as_deref().unwrap_or_default();
    let config = match input.config {
        Some(config) => Some(config),
        None => ctx.state.settings.read().await.view_at(&["textGenerators", id]),
    };
    let Some(config) = config else {
        return Err(Failure::text_generator("This text generator is not configured."));
    };
    let base = config.base_url.as_deref().unwrap_or_default().trim_end_matches('/').to_owned();
    let Some(key) = config.api_key.as_deref() else {
        return Err(Failure::text_generator("Add an API key (secret references are not supported on this server)."));
    };
    let anthropic = config.kind.as_deref() == Some("anthropic");
    let url = if anthropic { format!("{}/v1/models", if base.is_empty() { "https://api.anthropic.com" } else { &base }) } else { format!("{base}/models") };
    let auth = if anthropic { format!("x-api-key: {key}") } else { format!("authorization: Bearer {key}") };
    let mut args = vec!["-s", "-f", "--max-time", "15", "-H", &auth];
    if anthropic {
        args.extend(["-H", "anthropic-version: 2023-06-01"]);
    }
    args.push(&url);
    let (body, _) = curl(&args).await.map_err(|_| Failure::text_generator("Could not reach the endpoint or it rejected the key."))?;
    // The endpoint's own answer: a list under `data`, each model with an id and perhaps a name.
    let parsed: Value = serde_json::from_str(&body).map_err(|_| Failure::text_generator("The endpoint did not answer with a model list."))?;
    let models = parsed["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            Some(TextGeneratorModel { id: id.to_owned(), name: m["display_name"].as_str().or(m["name"].as_str()).unwrap_or(id).to_owned() })
        })
        .collect();
    Ok(TextGeneratorModels { models, tested_model: (input.test == Some(true)).then_some(input.model) })
}

/// The secret `server.checkSecretSource` is asked to read (`SecretReference`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct SecretSource {
    #[serde(deserialize_with = "lenient")]
    provider: Option<String>,
    #[serde(deserialize_with = "lenient")]
    reference: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CheckSecretInput {
    #[serde(deserialize_with = "fields_or_default")]
    source: SecretSource,
}

/// What this server reads of a secret provider's settings: its label, and the
/// driver and account it is read with.
#[derive(Default, Deserialize)]
#[serde(default)]
struct SecretProvider {
    #[serde(deserialize_with = "lenient")]
    label: Option<String>,
    #[serde(deserialize_with = "lenient")]
    driver: Option<String>,
    #[serde(deserialize_with = "lenient")]
    account: Option<String>,
}

/// Whether a secret could be read (`SecretSourceCheckResult`).
#[derive(Debug, Serialize)]
#[serde(tag = "_tag")]
enum SecretCheck {
    Resolved,
    Failed { detail: String },
}

/// `server.checkSecretSource`: reads one secret through its provider; never returns it.
async fn check_secret(ctx: Arc<ConnCtx>, input: CheckSecretInput) -> SecretCheck {
    let provider = input.source.provider.unwrap_or_default();
    let reference = input.source.reference.unwrap_or_default();
    // `driver` and `account` are keys the settings' record does not have: read from the document as it is.
    let def: SecretProvider = ctx.state.settings.read().await.view_at(&["secretProviders", &provider]).unwrap_or_default();
    let label = def.label.as_deref().unwrap_or(&provider).to_owned();
    let failed = || SecretCheck::Failed { detail: format!("Could not read this secret from \"{label}\". Check the provider's settings and that its store is unlocked.") };
    let driver = def.driver.as_deref().unwrap_or(&provider);
    if !driver.contains("onepassword") && driver != "1password" {
        return failed();
    }
    let mut args = vec!["--cache=false".to_owned(), "read".into(), "--no-newline".into()];
    if let Some(account) = &def.account {
        args.push(format!("--account={account}"));
    }
    args.push(reference);
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), crate::process::command("op").args(&args).kill_on_drop(true).output()).await;
    match out {
        Ok(Ok(o)) if o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty() => SecretCheck::Resolved,
        _ => failed(),
    }
}

/// Something a client says of itself, as the triage prompt shows it: a text or a whole number.
#[derive(Deserialize)]
#[serde(untagged)]
enum Shown {
    Text(String),
    Count(i64),
}

/// What a client says of itself for a triage prompt (`AuthClientPresentationMetadata`).
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TriageClient {
    #[serde(deserialize_with = "lenient")]
    surface: Option<Shown>,
    #[serde(deserialize_with = "lenient")]
    browser: Option<Shown>,
    #[serde(deserialize_with = "lenient")]
    os: Option<Shown>,
    #[serde(deserialize_with = "lenient")]
    os_major_version: Option<Shown>,
    #[serde(deserialize_with = "lenient")]
    device_type: Option<Shown>,
    #[serde(deserialize_with = "lenient")]
    app_version: Option<Shown>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TriageInput {
    #[serde(deserialize_with = "fields_or_default")]
    client: TriageClient,
}

/// `server.prepareTriage` (`ServerPrepareTriageResult`).
#[derive(Debug, Serialize)]
struct TriagePrompt {
    prompt: String,
}

/// `server.prepareTriage`: a prompt describing this environment for a bug report.
async fn prepare_triage(ctx: Arc<ConnCtx>, input: TriageInput) -> TriagePrompt {
    let env = &ctx.state.environment;
    let c = &input.client;
    let field = |v: &Option<Shown>| match v {
        Some(Shown::Text(text)) => text.clone(),
        Some(Shown::Count(n)) => n.to_string(),
        None => "unknown".into(),
    };
    let log_tail = std::fs::read_dir(&ctx.state.paths.logs)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .and_then(|e| std::fs::read_to_string(e.path()).ok())
        .map(|s| s.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    let prompt = format!(
        "Help me triage a problem with T3 Code.\n\n## Environment\n- Server: domovoi-server {} (t3code protocol)\n- Platform: {} {}\n- Client: {} {} on {} {} ({}), app {}\n\n## What happened\n<describe what you did, what you expected, and what happened instead>\n\n## Recent server log\n```\n{}\n```\n",
        env.server_version,
        env.platform.os,
        env.platform.arch,
        field(&c.surface),
        field(&c.browser),
        field(&c.os),
        field(&c.os_major_version),
        field(&c.device_type),
        field(&c.app_version),
        log_tail
    );
    TriagePrompt { prompt }
}

/// `server.getTraceDiagnostics`: the trace file and its rotated files, summarized.
async fn trace_diagnostics(ctx: &ConnCtx) -> crate::trace_diagnostics::TraceDiagnostics {
    let path = ctx.state.paths.logs.join(crate::trace_writer::TRACE_FILE);
    let read = path.clone();
    tokio::task::spawn_blocking(move || crate::trace_diagnostics::read(&read, crate::trace_writer::MAX_FILES, crate::trace_diagnostics::SLOW_SPAN_THRESHOLD_MS))
        .await
        .unwrap_or_else(|e| crate::trace_diagnostics::unreadable(&path, crate::trace_writer::MAX_FILES, e.to_string()))
}

/// The window of history a call asks for, in buckets (`ResourceTelemetryHistoryInput`):
/// five minutes in buckets of ten seconds unless it says otherwise.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct HistoryInput {
    #[serde(deserialize_with = "lenient")]
    window_ms: Option<i64>,
    #[serde(deserialize_with = "lenient")]
    bucket_ms: Option<i64>,
}

impl HistoryInput {
    fn window(&self) -> (i64, i64) {
        (self.window_ms.unwrap_or(300_000), self.bucket_ms.unwrap_or(10_000))
    }
}

/// What `server.signalProcess` is asked with (`ServerSignalProcessInput`):
/// pid 0, started at 0, and an interrupt unless it says otherwise.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SignalInput {
    #[serde(deserialize_with = "lenient")]
    pid: Option<i64>,
    #[serde(deserialize_with = "lenient")]
    start_time_ms: Option<i64>,
    #[serde(deserialize_with = "lenient")]
    signal: Option<Signal>,
}

/// `server.acceptAcpRegistryUrlAuth` (`AcpRegistryAcceptUrlAuthResult`).
#[derive(Debug, Serialize)]
struct UrlAuthAccepted {
    accepted: bool,
}

/// `AcpRegistryAcceptUrlAuthInput`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UrlAuthInput {
    instance_id: String,
    elicitation_id: String,
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = r
        .unary("server.getConfig", |ctx: Arc<ConnCtx>, _: serde::de::IgnoredAny| async move { Ok::<_, Never>(ctx.state.server_config().await) })
        // Only installed Swift TestFlight builds of t3code still ask; the settings are also in `server.getConfig`.
        .unary("server.getSettings", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(ctx.state.settings.read().await.clone()) })
        .unary("server.updateSettings", update_settings)
        .unary("server.upsertKeybinding", |ctx: Arc<ConnCtx>, i: UpsertInput| async move {
            crate::keybindings::upsert(&ctx.state.paths.keybindings(), &i).map(domovoi_model::typed::ServerUpsertKeybindingResult::from)
        })
        .unary("server.removeKeybinding", |ctx: Arc<ConnCtx>, i: KeybindingRule| async move {
            crate::keybindings::remove(&ctx.state.paths.keybindings(), &i).map(domovoi_model::typed::ServerRemoveKeybindingResult::from)
        })
        .unary("server.runStorageCleanup", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move {
            Ok::<_, Never>(run_cleanup_all(&ctx.state, StorageCleanupTrigger::Manual).await)
        })
        .stream("server.getStorageCleanupReport", |_ctx: Arc<ConnCtx>, _: IgnoredAny| cleanup_report_stream())
        .stream("server.getDatabaseStorage", |ctx: Arc<ConnCtx>, _: IgnoredAny| database_stream(ctx))
        .unary("server.reclaimDatabaseSpace", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move {
            ctx.state.store.db().with(|c| c.execute_batch("VACUUM;")).map_err(|e| Failure::DatabaseReclaim { reason: e.to_string() })
        })
        .unary("server.checkSecretSource", |ctx: Arc<ConnCtx>, Sent(i): Sent<CheckSecretInput>| async move { Ok::<_, Never>(check_secret(ctx, i).await) })
        .unary("server.checkOtlpEndpoint", |_ctx: Arc<ConnCtx>, Sent(i): Sent<OtlpCheckInput>| async move { Ok::<_, Never>(check_otlp(i).await) })
        .unary("server.listTextGeneratorModels", list_text_generator_models)
        .unary("server.prepareTriage", |ctx: Arc<ConnCtx>, Sent(i): Sent<TriageInput>| async move { Ok::<_, Never>(prepare_triage(ctx, i).await) })
        .unary("server.getUsageSummary", |ctx: Arc<ConnCtx>, super::shell::RecordOrNot(i): super::shell::RecordOrNot<crate::usage::SummaryInput>| async move {
            let settings = ctx.state.settings.read().await.clone();
            let env = crate::agent_sessions::ScanEnv::new(&settings, &ctx.state.paths.home);
            crate::usage::read_summary(env, i, (*settings).clone()).await
        })
        .unary("server.refreshUsageRates", |_ctx: Arc<ConnCtx>, _: serde::de::IgnoredAny| async move { Ok::<_, super::Never>(crate::usage::refresh_rates().await) })
        .unary("server.getTraceDiagnostics", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(trace_diagnostics(&ctx).await) })
        .unary("server.getProcessDiagnostics", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(super::telemetry::process_diagnostics().await) })
        .unary("server.getHostResources", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(super::telemetry::host_resources().await) })
        .unary("server.getProcessResourceHistory", |_ctx: Arc<ConnCtx>, Sent(i): Sent<HistoryInput>| async move {
            let (window_ms, bucket_ms) = i.window();
            Ok::<_, Never>(super::telemetry::process_history(window_ms, bucket_ms))
        })
        .unary("server.getResourceTelemetryHistory", |_ctx: Arc<ConnCtx>, Sent(i): Sent<HistoryInput>| async move {
            let (window_ms, bucket_ms) = i.window();
            Ok::<_, Never>(super::telemetry::telemetry_history(window_ms, bucket_ms))
        })
        .unary("server.retryResourceTelemetry", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(super::telemetry::retry().await) })
        .unary("server.signalProcess", |_ctx: Arc<ConnCtx>, Sent(i): Sent<SignalInput>| async move {
            Ok::<_, Never>(super::telemetry::signal_process(i.pid.unwrap_or(0), i.start_time_ms.unwrap_or(0), i.signal.unwrap_or(Signal::Interrupt)).await)
        })
        // Domovoi is not published to npm: there is no release to name.
        .unary("server.getNpmRelease", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Ok::<_, Never>(None::<Never>) })
        .unary("server.updateServer", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Err::<Never, _>(Failure::self_update()) })
        .stream("server.updateServerWithProgress", |_ctx: Arc<ConnCtx>, _: IgnoredAny| futures::stream::once(async { Err::<Never, _>(Failure::self_update()) }))
        .unary("server.commitDesktopUpdate", |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Err::<Never, _>(Failure::self_update()) })
        .unary("server.updateProvider", |ctx: Arc<ConnCtx>, i: crate::provider_update::UpdateInput| async move { crate::provider_update::update(&ctx.state, i).await });
    // The user opened the sign-in an ACP agent asked for (`elicitation/create`, mode `url`).
    let r = r.unary("server.acceptAcpRegistryUrlAuth", |_ctx: Arc<ConnCtx>, Sent(i): Sent<UrlAuthInput>| async move {
        Ok::<_, Never>(UrlAuthAccepted { accepted: crate::adapters::acp::url_auth::accept(&i.instance_id, &i.elicitation_id) })
    });
    let r = r
        .unary("server.searchAcpRegistry", |_ctx: Arc<ConnCtx>, i: AcpSearchInput| async move {
            crate::acp_registry::search(&i.query, i.limit).await.map(|agents| AcpSearchResult { agents })
        })
        .unary("server.prepareAcpRegistryAgent", |_ctx: Arc<ConnCtx>, i: AcpAgentInput| async move { crate::acp_registry::prepare(&i.agent_id, None).await })
        .unary("server.uninstallAcpRegistryManagedBinary", |_ctx: Arc<ConnCtx>, i: AcpAgentInput| async move { crate::acp_registry::uninstall(&i.agent_id) });
    use crate::rpc::admin_failure::AcpRegistryReason as Why;
    [
        ("server.listAcpRegistrySessions", Why::SessionListUnsupported, "Domovoi does not list an ACP agent's own sessions; its threads continue their sessions."),
        ("server.importAcpRegistrySession", Why::SessionListUnsupported, "Domovoi does not import an ACP agent's own sessions."),
        ("server.deleteAcpRegistrySession", Why::SessionDeleteUnsupported, "Domovoi does not delete an ACP agent's own sessions."),
        ("server.listAcpRegistryProviders", Why::ProvidersUnsupported, "Domovoi does not configure an ACP agent's model providers; use the agent's own settings."),
        ("server.setAcpRegistryProvider", Why::ProvidersUnsupported, "Domovoi does not configure an ACP agent's model providers; use the agent's own settings."),
        ("server.disableAcpRegistryProvider", Why::ProvidersUnsupported, "Domovoi does not configure an ACP agent's model providers; use the agent's own settings."),
        ("server.logoutAcpRegistry", Why::LogoutUnsupported, "Sign the ACP agent out with its own command."),
    ]
    .into_iter()
    .fold(r, |r, (tag, why, message)| r.unary(tag, move |_ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Err::<Never, _>(Failure::acp_unsupported(why, message)) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn git(cwd: &std::path::Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git").args(args).current_dir(cwd).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    /// A pushed project checkout (`root`, tracking `remote`) with one commit.
    fn pushed_repo(d: &std::path::Path) -> std::path::PathBuf {
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        let root = d.join("root");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "a"]);
        git(&root, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&root, &["push", "-q", "-u", "origin", "main"]);
        root
    }

    #[tokio::test]
    async fn worktrees_go_only_when_a_rule_allows_and_nothing_is_unsaved() {
        use crate::db::Db;
        let d = std::env::temp_dir().join(format!("domovoi-wt-clean-{}", crate::util::new_uuid()));
        let root = pushed_repo(&d);
        let wt = d.join("wt");
        git(&root, &["worktree", "add", "-q", "-b", "domovoi/x", wt.to_str().unwrap()]);
        let store = crate::store::Store::new(Db::open_in_memory().unwrap());
        store.create_project(domovoi_contracts::shell::ProjectShell {
            id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: root.to_string_lossy().into(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
            favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
            created_at: "t".into(), updated_at: "t".into(),
        }, None).unwrap();
        let settled = domovoi_contracts::time_fmt::iso_from_millis(domovoi_contracts::time_fmt::now_millis() - 3 * 86_400_000);
        let thread = crate::test_records::with(crate::test_records::thread("t1", "p"), json!({ "title": "Done", "worktreePath": wt,
            "settledOverride": "settled", "settledAt": settled, "updatedAt": settled }));
        crate::test_records::append(&store, "t1", "thread.created", thread, &Default::default()).unwrap();
        let none = d.join("none");
        let run = |rules: Value| { let store = store.clone(); let none = none.clone(); async move { worktree_entries(&store, &Rules::of(&rules), &none).await } };

        let (e, _) = run(json!({})).await;
        assert_eq!((&e[0].outcome, &e[0].reason[..]), (&StorageCleanupOutcome::Kept, "No storage cleanup rule removes worktrees."));
        let (e, _) = run(json!({ "worktreeSettledAfterDays": 7 })).await;
        assert_eq!(e[0].reason, "Settled for 3 of 7 days.");
        // As the client is sent it: no project path and no file count.
        assert_eq!(
            json!(e[0]),
            json!({ "kind": "worktree", "outcome": "kept", "reason": "Settled for 3 of 7 days.", "path": wt, "threadId": "t1", "threadTitle": "Done",
                "bytes": dir_bytes(&wt) })
        );
        let (e, _) = run(json!({ "worktreeAfterDays": 30 })).await;
        assert_eq!(e[0].reason, "No storage cleanup rule applies to it yet.");
        let (e, _) = run(json!({ "worktreeAfterDays": "soon" })).await;
        assert_eq!(e[0].reason, "No storage cleanup rule applies to it yet.", "a rule that is set, though to no count of days");

        std::fs::write(wt.join("scratch.txt"), "x").unwrap();
        let rules = json!({ "worktreeSettledAfterDays": 2, "worktreeKeepWhen": "uncommitted-changes" });
        let (e, _) = run(rules.clone()).await;
        assert_eq!(e[0].reason, "It has local changes.");
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-qm", "local"]);
        let (e, _) = run(rules.clone()).await;
        assert_eq!(e[0].reason, "It has commits that are not pushed or in the project checkout.");
        git(&wt, &["push", "-q", "origin", "domovoi/x"]);
        let (e, freed) = run(rules).await;
        assert_eq!(e[0].outcome, StorageCleanupOutcome::Removed, "{e:?}");
        assert!(freed > 0 && !wt.exists());
        // A folder under the worktrees directory that no thread uses is listed, and kept.
        let stray = d.join("worktrees").join("repo").join("left");
        std::fs::create_dir_all(&stray).unwrap();
        let (e, _) = worktree_entries(&store, &Rules::default(), &d.join("worktrees")).await;
        assert_eq!(
            json!(e),
            json!([{ "kind": "worktree", "outcome": "kept", "reason": "No thread on this server uses it.", "path": stray, "threadId": null, "threadTitle": null }])
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn auto_pull_takes_only_a_clean_default_branch_with_nothing_local() {
        let d = std::env::temp_dir().join(format!("domovoi-autopull-{}", crate::util::new_uuid()));
        let root = pushed_repo(&d);
        let other = d.join("other");
        git(&d, &["clone", "-q", d.join("remote.git").to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(other.join("b.txt"), "b").unwrap();
        git(&other, &["add", "."]);
        git(&other, &["commit", "-qm", "b"]);
        git(&other, &["push", "-q"]);
        let cwd = root.to_str().unwrap();
        assert_eq!(crate::vcs_ops::auto_pull_blocker(cwd).await, None);
        std::fs::write(root.join("a.txt"), "changed").unwrap();
        assert_eq!(crate::vcs_ops::auto_pull_blocker(cwd).await, Some("working-tree-changes"));
        git(&root, &["checkout", "-q", "a.txt"]);
        let project = domovoi_contracts::shell::ProjectShell {
            id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: cwd.into(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: true,
            favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
            created_at: "t".into(), updated_at: "t".into(),
        };
        crate::vcs_ops::auto_pull_projects(&[project], &domovoi_model::typed::ServerSettings::default()).await;
        assert!(root.join("b.txt").exists(), "pulled");
        git(&root, &["commit", "--allow-empty", "-qm", "mine"]);
        assert_eq!(crate::vcs_ops::auto_pull_blocker(cwd).await, Some("local-commits"));
        git(&root, &["checkout", "-q", "-b", "feature"]);
        assert_eq!(crate::vcs_ops::auto_pull_blocker(cwd).await, Some("not-on-default-branch"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cleanup_removes_only_old_logs() {
        let d = std::env::temp_dir().join(format!("domovoi-clean-{}", crate::util::new_uuid()));
        let logs = d.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("new.log"), "x").unwrap();
        let old = logs.join("old.log");
        std::fs::write(&old, "xyz").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(past).unwrap();
        let r = publish(StorageCleanupTrigger::Manual, run_cleanup_logs(&logs, LOG_RETENTION_DAYS));
        assert!(!old.exists() && logs.join("new.log").exists());
        // As the client is sent it.
        assert_eq!(
            json!(r),
            json!({ "trigger": "manual", "startedAt": r.started_at, "finishedAt": r.finished_at, "omittedCount": 0, "bytesFreed": 3,
                "counts": { "removed": 1, "kept": 0, "failed": 0 },
                "entries": [{ "kind": "logs", "outcome": "removed", "reason": "Older than 14 days.", "path": old, "threadId": null, "threadTitle": null, "bytes": 3, "files": 1 }] })
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rules_and_payloads_are_read_leniently() {
        let rules = Rules::of(&json!({ "logsAfterDays": 3, "worktreeAfterDays": 1.5, "worktreeSettledAfterDays": "7", "worktreeOnDelete": "yes", "worktreeKeepWhen": "tracked-changes" }));
        assert_eq!((rules.logs_after_days, rules.worktree_after_days, rules.worktree_settled_after_days), (Some(3), Some(None), None));
        assert_eq!((rules.worktree_on_delete, rules.worktree_unchanged, rules.keep_when), (None, None, KeepWhen::TrackedChanges));
        let unset = Rules::of(&json!({ "worktreeAfterDays": null, "worktreeKeepWhen": 5 }));
        assert_eq!((unset.worktree_after_days, unset.keep_when), (None, KeepWhen::UncommittedChanges));
        assert_eq!(Rules::of(&json!(null)).keep_when, KeepWhen::UncommittedChanges);

        let window = |payload: Value| serde_json::from_value::<Sent<HistoryInput>>(payload).unwrap().0.window();
        assert_eq!(window(json!({ "windowMs": 60_000, "bucketMs": 5_000 })), (60_000, 5_000));
        assert_eq!(window(json!({ "windowMs": "soon", "bucketMs": 0.5 })), (300_000, 10_000));
        for no_object in [json!(null), json!([60_000, 5_000]), json!("x")] {
            assert_eq!(window(no_object), (300_000, 10_000));
        }
        let Sent(signal) = serde_json::from_value::<Sent<SignalInput>>(json!({ "pid": 7, "startTimeMs": 9, "signal": "SIGKILL" })).unwrap();
        assert_eq!((signal.pid, signal.start_time_ms, signal.signal), (Some(7), Some(9), Some(Signal::Kill)));
        let Sent(signal) = serde_json::from_value::<Sent<SignalInput>>(json!({ "pid": "7", "signal": 9 })).unwrap();
        assert_eq!((signal.pid, signal.start_time_ms, signal.signal), (None, None, None));
        let Sent(models) = serde_json::from_value::<Sent<ListModelsInput>>(json!({ "id": "a", "config": "none", "test": 1 })).unwrap();
        assert!(models.config.is_none() && models.test.is_none() && models.id.as_deref() == Some("a"));
    }

    #[test]
    fn answers_are_written_as_the_client_reads_them() {
        assert_eq!(json!(OtlpCheck::Accepted { latency_ms: 12.5 }), json!({ "_tag": "Accepted", "latencyMs": 12.5 }));
        assert_eq!(json!(OtlpCheck::Rejected { status: 503 }), json!({ "_tag": "Rejected", "status": 503 }));
        assert_eq!(json!(OtlpCheck::Unreachable { timed_out: true }), json!({ "_tag": "Unreachable", "timedOut": true }));
        assert_eq!(json!(SecretCheck::Resolved), json!({ "_tag": "Resolved" }));
        assert_eq!(json!(SecretCheck::Failed { detail: "no".into() }), json!({ "_tag": "Failed", "detail": "no" }));
        let models = |tested_model| TextGeneratorModels { models: vec![TextGeneratorModel { id: "m".into(), name: "M".into() }], tested_model };
        assert_eq!(json!(models(None)), json!({ "models": [{ "id": "m", "name": "M" }] }));
        assert_eq!(json!(models(Some(None))), json!({ "models": [{ "id": "m", "name": "M" }], "testedModel": null }));
        assert_eq!(json!(models(Some(Some("m".into()))))["testedModel"], "m");
        assert_eq!(json!(UrlAuthAccepted { accepted: false }), json!({ "accepted": false }));
        assert_eq!(json!(None::<Never>), json!(null));
    }
}
