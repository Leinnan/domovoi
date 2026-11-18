//! Orchestration read RPCs and the two stop controls, after t3code's
//! `ThreadManagementService`, `ProjectionStore`, `ThreadFind`, `ThreadSearch`
//! and `CommandOutputHub`.
//!
//! Simplifications, all invisible on the wire:
//! - find and search match the raw message text; t3code first strips markdown
//!   syntax (`searchableMessageSegments`), so `**` or link targets can match here;
//! - search ranks by match count then recency, without t3code's pick history;
//! - native child work exists only for Antigravity, which Domovoi does not run.

use super::shell::text_or_empty;
use crate::orchestration::failure::{Failure, ForceStopReason, StopWorkflowReason, WorkflowScriptReason};
use crate::orchestration::projection;
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use domovoi_contracts::shell::ProjectShell;
use domovoi_model::typed::command::RunInterrupt;
use domovoi_model::typed::{
    AppThread, EventBody, MessageId, ProjectId, Run, RunId, RunStatus, Subagent, SubagentStatus, ThreadId, ThreadShell, Timestamp, TurnItem,
    TurnItemStatus,
};
use tokio::sync::broadcast::error::RecvError;

/// What Domovoi has none of: the type of a list that is always empty, or of a
/// value that is always absent.
#[derive(Debug, Clone, Copy)]
enum Nothing {}

impl Serialize for Nothing {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        match *self {}
    }
}

/// Live (not deleted) thread records.
fn live_threads(state: &AppState) -> Result<Vec<AppThread>, Failure> {
    Ok(state.store.app_threads().map_err(Failure::shell)?.into_iter().filter(|t| t.deleted_at.is_none()).collect())
}

/// Shells as of a sequence (`OrchestrationV2ThreadFamilySnapshot` and its like).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellRows {
    snapshot_sequence: u64,
    threads: Vec<ThreadShell>,
}

/// The shells whose records pass `keep`, with the sequence they are as of.
fn shell_rows(state: &AppState, keep: impl Fn(&AppThread) -> bool) -> Result<ShellRows, Failure> {
    let snapshot_sequence = state.store.latest_sequence().map_err(Failure::shell)?;
    let mut threads = vec![];
    for record in state.store.app_threads().map_err(Failure::shell)? {
        if !keep(&record) {
            continue;
        }
        if let Some(shell) = projection::thread_shell(&state.store, record.id.as_str()).map_err(Failure::shell)? {
            threads.push(shell);
        }
    }
    Ok(ShellRows { snapshot_sequence, threads })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadIds {
    thread_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadOnly {
    thread_id: String,
}

/// What a link to a thread shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkMetadata {
    title: String,
    project_id: ProjectId,
}

/// The live threads among `ids`, each with what `of` takes from its record.
fn of_threads<T>(state: &AppState, ids: &[String], of: impl Fn(AppThread) -> T) -> Result<BTreeMap<String, T>, Failure> {
    Ok(live_threads(state)?.into_iter().filter(|t| ids.iter().any(|wanted| wanted == t.id.as_str())).map(|t| (t.id.as_str().to_owned(), of(t))).collect())
}

fn get_thread_family(state: &AppState, thread_id: &str) -> Result<ShellRows, Failure> {
    // The member may be deleted: forks keep a deleted source's root.
    let member = state.store.app_thread(thread_id).map_err(Failure::shell)?;
    // The root of a thread's lineage: forks and subagents share it.
    let Some(root) = member.map(|member| member.lineage.root_thread_id) else {
        return Ok(ShellRows { snapshot_sequence: state.store.latest_sequence().map_err(Failure::shell)?, threads: vec![] });
    };
    shell_rows(state, |t| t.deleted_at.is_none() && t.lineage.root_thread_id == root)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnItemInput {
    thread_id: String,
    item_id: String,
}

/// `orchestration.getTurnItem`: the item, or `null` for one the thread does not have.
#[derive(Serialize)]
struct TurnItemAnswer {
    item: Option<TurnItem>,
}

fn turn_item(state: &AppState, thread_id: &str, item_id: &str) -> Result<Option<TurnItem>, Failure> {
    state.store.record::<TurnItem>(thread_id, item_id).map_err(|e| Failure::projection(thread_id, e))
}

// ---- find in thread ----

/// `foldThreadSearchCase`: lower case, final sigma and curly apostrophes folded.
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let lower: Vec<char> = c.to_lowercase().collect();
            if lower.len() == 1 { lower } else { vec![c] }
        })
        .map(|c| match c {
            'ς' => 'σ',
            '\u{2018}' | '\u{2019}' => '\'',
            c => c,
        })
        .collect()
}

/// Non-overlapping occurrences, as byte offsets into the folded text.
fn occurrences(text: &str, query: &str) -> Vec<usize> {
    if text.is_empty() || query.is_empty() {
        return vec![];
    }
    let (t, q) = (fold(text), fold(query));
    let mut out = vec![];
    let mut cursor = 0;
    while let Some(off) = t[cursor..].find(&q) {
        out.push(cursor + off);
        cursor += off + q.len();
    }
    out
}

fn searchable_text(item: &TurnItem) -> Option<&str> {
    match item {
        TurnItem::UserMessage(message) => Some(&message.text),
        TurnItem::AssistantMessage(message) => Some(&message.text),
        TurnItem::ProposedPlan(plan) => Some(&plan.markdown),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindInput {
    thread_id: String,
    query: String,
    index: Option<i64>,
    offset: Option<i64>,
    start: Option<FindStart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindStart {
    entry_id: String,
    occurrence: i64,
}

struct Doc {
    entry_id: String,
    run_id: Option<RunId>,
    count: i64,
}

/// An entry of the thread with matches in it, and where its matches start among all of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FindEntry {
    entry_id: String,
    run_id: Option<RunId>,
    start_index: i64,
    count: i64,
}

/// The match a search is on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FindMatch {
    entry_id: String,
    run_id: Option<RunId>,
    occurrence: i64,
}

/// `OrchestrationV2SearchThreadResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FindResult {
    complete: bool,
    snapshot_sequence: u64,
    total_matches: i64,
    active_index: i64,
    navigation: Vec<FindEntry>,
    #[serde(rename = "match")]
    found: Option<FindMatch>,
}

/// `selectMatch` + `resultForSelection`.
fn find_result(docs: &[Doc], input: &FindInput, seq: u64) -> FindResult {
    let total: i64 = docs.iter().map(|d| d.count).sum();
    let mut requested = input.index.unwrap_or(0);
    if input.index.is_none() {
        if let Some(start) = &input.start {
            if let Some(i) = docs.iter().position(|d| d.entry_id == start.entry_id) {
                requested = docs[..i].iter().map(|d| d.count).sum::<i64>() + start.occurrence.min(docs[i].count);
                if requested >= total {
                    requested = 0;
                }
            }
        }
    }
    let active = if input.index.is_none() && total > 0 {
        (requested + input.offset.unwrap_or(0)).rem_euclid(total)
    } else {
        requested.min((total - 1).max(0))
    };
    let mut occurrence = active;
    let mut selected = None;
    for d in docs {
        if occurrence < d.count {
            selected = Some(d);
            break;
        }
        occurrence -= d.count;
    }
    let mut start = 0;
    let entries: Vec<FindEntry> = docs
        .iter()
        .filter_map(|d| {
            let e = FindEntry { entry_id: d.entry_id.clone(), run_id: d.run_id.clone(), start_index: start, count: d.count };
            start += d.count;
            (d.count > 0).then_some(e)
        })
        .collect();
    let sel = entries.iter().position(|e| Some(e.entry_id.as_str()) == selected.map(|d| d.entry_id.as_str()));
    let navigation: Vec<FindEntry> = if entries.len() <= 17 {
        entries
    } else {
        let n = entries.len() as i64;
        let s = sel.map(|s| s as i64).unwrap_or(-1);
        (0..17).map(|i| entries[((s + i - 8).rem_euclid(n)) as usize].clone()).collect()
    };
    FindResult {
        complete: true,
        snapshot_sequence: seq,
        total_matches: total,
        active_index: active,
        navigation,
        found: selected.map(|d| FindMatch { entry_id: d.entry_id.clone(), run_id: d.run_id.clone(), occurrence }),
    }
}

fn search_thread(state: &AppState, input: &FindInput) -> Result<FindResult, Failure> {
    let err = |cause: String| Failure::SearchThread { cause };
    let seq = state.store.latest_sequence().map_err(|e| err(e.to_string()))?;
    let thread = projection::thread(&state.store, &input.thread_id)
        .map_err(|e| err(e.to_string()))?
        .filter(|t| t.record.deleted_at.is_none())
        .ok_or_else(|| err(format!("Thread {} was not found.", input.thread_id)))?;
    let docs: Vec<Doc> = thread
        .visible_turn_item_refs()
        .into_iter()
        .filter_map(|item| {
            let text = searchable_text(item)?;
            let entry_id = item.message_id().map_or(item.id().as_str(), |id| id.as_str()).to_owned();
            Some(Doc { entry_id, run_id: item.run_id().cloned(), count: occurrences(text, &input.query).len() as i64 })
        })
        .collect();
    Ok(find_result(&docs, input, seq))
}

// ---- search across threads ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchThreadsInput {
    query: String,
    limit: Option<usize>,
    older_settled_before: Option<String>,
    project_ids: Option<Vec<String>>,
    #[serde(default)]
    titles_only: bool,
}

const SNIPPET_LENGTH: usize = 240;
const SNIPPET_LEAD: usize = 24;

/// `buildSearchSnippet`, on char boundaries.
fn snippet(text: &str, match_start: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let match_char = text[..match_start.min(text.len())].chars().count();
    let mut start = match_char.saturating_sub(SNIPPET_LEAD);
    if start > 0 {
        if let Some(ws) = chars[start..match_char].iter().position(|c| c.is_whitespace()) {
            start += ws + 1;
        }
    }
    let raw_end = chars.len().min(start + SNIPPET_LENGTH * 2);
    let raw: String = chars[start..raw_end].iter().collect();
    let cleaned = raw.replace("**", "").replace('`', "");
    let shown: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let shown_chars: Vec<char> = shown.chars().collect();
    let end = shown_chars.len().min(SNIPPET_LENGTH - 2);
    let cut = end < shown_chars.len() || raw_end < chars.len();
    format!("{}{}{}", if start > 0 { "…" } else { "" }, shown_chars[..end].iter().collect::<String>(), if cut { "…" } else { "" })
}

/// A thread with messages that match a search, shown by its newest one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadMatch {
    thread_id: ThreadId,
    project_id: ProjectId,
    message_id: Option<MessageId>,
    source: &'static str,
    snippet: String,
    message_created_at: Timestamp,
    quality: u8,
    /// How many of the thread's messages match.
    hits: usize,
}

/// `OrchestrationSearchThreadsResult`: the matches, and (when asked for) the
/// older settled threads whose title matches.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsFound {
    matches: Vec<ThreadMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threads: Option<Vec<ThreadShell>>,
}

fn search_threads(state: &AppState, input: &SearchThreadsInput) -> Result<ThreadsFound, Failure> {
    let err = |message: String| Failure::SearchThreads { message };
    let in_scope = |t: &AppThread| t.deleted_at.is_none() && input.project_ids.as_ref().is_none_or(|ps| ps.iter().any(|p| t.project_id.as_str() == p));
    let mut matches = vec![];
    if !input.titles_only && !input.query.trim().is_empty() {
        for record in state.store.app_threads().map_err(|e| err(e.to_string()))? {
            if !in_scope(&record) || record.archived_at.is_some() {
                continue;
            }
            let Some(thread) = projection::thread(&state.store, record.id.as_str()).map_err(|e| err(e.to_string()))? else { continue };
            let mut best: Option<ThreadMatch> = None;
            let mut hits = 0usize;
            for item in thread.visible_turn_item_refs() {
                let (source, text) = match item {
                    TurnItem::UserMessage(message) => ("user", message.text.as_str()),
                    TurnItem::AssistantMessage(message) => ("assistant", message.text.as_str()),
                    _ => continue,
                };
                let found = occurrences(text, &input.query);
                if found.is_empty() {
                    continue;
                }
                hits += 1;
                // Newest matching message wins (items are in ordinal order).
                best = Some(ThreadMatch {
                    thread_id: record.id.clone(),
                    project_id: record.project_id.clone(),
                    message_id: item.message_id().cloned(),
                    source,
                    snippet: snippet(&fold(text), found[0]),
                    message_created_at: item.started_at().unwrap_or(item.updated_at()).clone(),
                    quality: 1,
                    hits: 0,
                });
            }
            if let Some(mut found) = best {
                found.hits = hits;
                matches.push(found);
            }
        }
        matches.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| b.message_created_at.as_str().cmp(a.message_created_at.as_str())));
        matches.truncate(input.limit.unwrap_or(50));
    }
    let mut out = ThreadsFound { matches, snapshot_sequence: None, threads: None };
    if let Some(before) = &input.older_settled_before {
        // Older settled threads whose title matches: the rows a client's shell omits.
        let q = fold(&input.query);
        let rows = shell_rows(state, |t| {
            in_scope(t)
                && t.archived_at.is_none()
                && t.settled_at.as_ref().and_then(Option::as_ref).is_some_and(|settled| settled.as_str() < before.as_str())
                && fold(&t.title).contains(&q)
        })?;
        out.snapshot_sequence = Some(rows.snapshot_sequence);
        out.threads = Some(rows.threads);
    }
    Ok(out)
}

// ---- stop controls ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForceStopInput {
    thread_id: String,
    run_id: Option<String>,
}

/// A run a force stop interrupts: under way, or about to be.
fn is_active_run(run: &Run) -> bool {
    matches!(run.status.as_str(), "starting" | "running" | "waiting" | "interrupting" | "pending")
}

/// `OrchestrationV2ForceStopThreadResult`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForceStopped {
    outcome: ForceStopOutcome,
    shared_with_thread_ids: [ThreadId; 0],
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum ForceStopOutcome {
    /// The thread's own agent process was ended.
    Killed,
    /// Nothing of the thread's was live: its run is let go.
    Released,
}

async fn force_stop(ctx: Arc<ConnCtx>, input: ForceStopInput) -> Result<ForceStopped, Failure> {
    let err = |reason: Option<ForceStopReason>| Failure::ForceStopThread { thread_id: input.thread_id.clone(), reason };
    let state = &ctx.state;
    let runs = state.store.records::<Run>(&input.thread_id).map_err(|_| err(None))?;
    // The active run, else the latest that started; queued follow-ups never ran.
    let target = runs.iter().find(|run| is_active_run(run)).or_else(|| runs.iter().filter(|run| run.status != RunStatus::Queued).max_by_key(|run| run.ordinal));
    if let Some(wanted) = &input.run_id {
        if target.map(|run| run.id.as_str()) != Some(wanted.as_str()) {
            return Err(err(Some(ForceStopReason::Superseded)));
        }
    }
    let killed = match crate::adapters::HOST.get() {
        Some(host) => host.force_stop(&input.thread_id).await,
        None => false,
    };
    if let Some(run) = target.filter(|run| is_active_run(run)) {
        let interrupt = RunInterrupt {
            command_id: format!("server:force-stop:{}:{}", run.id, domovoi_contracts::time_fmt::now_millis()).into(),
            thread_id: input.thread_id.as_str().into(),
            run_id: Some(run.id.clone()),
            reason: Some("force-stop".into()),
            hold_queue: None,
        };
        let _ = state.orchestrator.dispatch(interrupt.into()).await;
    }
    Ok(ForceStopped { outcome: if killed { ForceStopOutcome::Killed } else { ForceStopOutcome::Released }, shared_with_thread_ids: [] })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopWorkflowInput {
    thread_id: String,
    subagent_id: String,
}

/// `orchestration.stopWorkflow`. Domovoi stops none: the answer is always why not.
async fn stop_workflow(ctx: Arc<ConnCtx>, input: StopWorkflowInput) -> Result<Nothing, Failure> {
    let err = |reason: StopWorkflowReason| Failure::StopWorkflow { thread_id: input.thread_id.clone(), subagent_id: input.subagent_id.clone(), reason };
    let record = ctx.state.store.app_thread(&input.thread_id).map_err(|_| err(StopWorkflowReason::Unavailable))?;
    if record.is_none_or(|t| t.deleted_at.is_some()) {
        return Err(err(StopWorkflowReason::Unavailable));
    }
    let task = ctx.state.store.record::<Subagent>(&input.thread_id, &input.subagent_id).map_err(|_| err(StopWorkflowReason::Unavailable))?;
    match task {
        Some(t) if t.thread_id.as_str() == input.thread_id && t.workflow.is_some() && t.status == SubagentStatus::Running => {
            // Only Claude's native workflows can be stopped, and Domovoi's Claude
            // adapter does not surface workflows as subagents.
            Err(err(StopWorkflowReason::Unsupported))
        }
        _ => Err(err(StopWorkflowReason::NotRunning)),
    }
}

// ---- workflow scripts ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowScriptInput {
    script_path: String,
}

/// `OrchestrationGetWorkflowScriptResult`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowScript {
    script_path: String,
    contents: String,
    truncated: bool,
}

/// `readContainedWorkflowFile`: an absolute `.js` file under `~/.claude/projects`.
fn read_workflow_script(script_path: &str) -> Result<WorkflowScript, Failure> {
    use WorkflowScriptReason as Reason;
    let err = |reason: Reason, path: &str| Failure::WorkflowScript { reason, script_path: path.to_owned() };
    let path = std::path::Path::new(script_path);
    if !path.is_absolute() || path.extension().and_then(|e| e.to_str()) != Some("js") {
        return Err(err(Reason::InvalidPath, script_path));
    }
    let config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok().filter(|s| !s.trim().is_empty());
    let base = config_dir.map(std::path::PathBuf::from).unwrap_or_else(|| crate::util::home_dir().join(".claude"));
    let root = std::fs::canonicalize(base.join("projects")).map_err(|_| err(Reason::RootUnavailable, script_path))?;
    let resolved = std::fs::canonicalize(path).map_err(|_| err(Reason::NotFound, script_path))?;
    let shown = resolved.to_string_lossy().into_owned();
    if !resolved.starts_with(&root) {
        return Err(err(Reason::OutsideRoot, &shown));
    }
    if resolved.extension().and_then(|e| e.to_str()) != Some("js") {
        return Err(err(Reason::NotJs, &shown));
    }
    let meta = std::fs::metadata(&resolved).map_err(|_| err(Reason::ReadFailed, &shown))?;
    if !meta.is_file() {
        return Err(err(Reason::NotRegularFile, &shown));
    }
    const CAP: usize = 256 * 1024;
    let bytes = std::fs::read(&resolved).map_err(|_| err(Reason::ReadFailed, &shown))?;
    let truncated = bytes.len() > CAP;
    let mut end = bytes.len().min(CAP);
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    Ok(WorkflowScript { script_path: shown, contents: String::from_utf8_lossy(&bytes[..end]).into_owned(), truncated })
}

// ---- command output ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommandOutputInput {
    thread_id: String,
    item_id: String,
}

/// Whether work in this status may still produce output.
fn is_work_active(status: &TurnItemStatus) -> bool {
    matches!(status, TurnItemStatus::Idle | TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting)
}

/// What a command has printed, in place of whatever was sent before
/// (`OrchestrationV2CommandOutputFrame`, of which Domovoi sends only `replace`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename = "replace")]
struct OutputFrame {
    text: String,
    truncated: bool,
    running: bool,
}

/// A `replace` frame for a command item's persisted output.
fn output_frame(item: &TurnItem) -> OutputFrame {
    const MAX: usize = 64 * 1024;
    let out = match item {
        TurnItem::CommandExecution(command) => command.output.as_deref().unwrap_or_default(),
        _ => "",
    };
    let (text, truncated) = if out.len() > MAX {
        let mut cut = out.len() - MAX;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        let tail = &out[cut..];
        (tail.split_once('\n').map(|(_, rest)| rest).unwrap_or(tail).to_owned(), true)
    } else {
        (out.to_owned(), false)
    };
    OutputFrame { text, truncated, running: is_work_active(item.status()) }
}

/// Output frames for one command item until it settles. Domovoi's adapters record
/// output when a command finishes, so frames follow `turn-item.updated` events.
fn command_output(ctx: Arc<ConnCtx>, input: CommandOutputInput) -> BoxStream<'static, Result<OutputFrame, Failure>> {
    let store = ctx.state.store.clone();
    async_stream::stream! {
        let err = |m: &str| Failure::CommandOutput { thread_id: input.thread_id.clone(), message: m.to_owned() };
        let mut live = store.subscribe();
        let item = match turn_item(&ctx.state, &input.thread_id, &input.item_id) {
            Ok(Some(item @ TurnItem::CommandExecution(_))) => item,
            Ok(_) => { yield Err(err("No such command.")); return; }
            Err(_) => { yield Err(err("Failed to read command output.")); return; }
        };
        let first = output_frame(&item);
        let mut last = first.clone();
        yield Ok(first);
        if !last.running { return; }
        loop {
            match live.recv().await {
                Ok(e) if e.aggregate_id == input.thread_id => {
                    let Some(EventBody::TurnItemUpdated(item)) = e.thread_event.as_ref().map(|event| &event.body) else { continue };
                    if item.id().as_str() != input.item_id { continue }
                    let frame = output_frame(item);
                    if frame != last {
                        last = frame.clone();
                        yield Ok(frame);
                    }
                    if !last.running { return; }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Ok(Some(item)) = turn_item(&ctx.state, &input.thread_id, &input.item_id) {
                        let frame = output_frame(&item);
                        last = frame.clone();
                        yield Ok(frame);
                        if !last.running { return; }
                    }
                }
                Err(RecvError::Closed) => { yield Err(err("Lost the thread's event stream.")); return; }
            }
        }
    }
    .boxed()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeInput {
    #[serde(default, deserialize_with = "text_or_empty")]
    worktree_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeThreadIds {
    thread_ids: Vec<ThreadId>,
}

/// `orchestration.getArchivedShellSnapshot`: the shell snapshot with the archived threads as its threads.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchivedShellSnapshot {
    schema_version: u32,
    snapshot_sequence: u64,
    projects: Vec<ProjectShell>,
    threads: Vec<Arc<ThreadShell>>,
}

/// `orchestration.getNativeChildWork`: Domovoi runs no provider with native child work.
#[derive(Serialize)]
struct NativeChildWork {
    children: [Nothing; 0],
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("orchestration.getThreadProjection", |ctx: Arc<ConnCtx>, input: ThreadOnly| async move {
        crate::orchestration::stream::snapshot(&ctx.state.store, &input.thread_id).map(|snapshot| snapshot.projection)
    })
    .unary("orchestration.getThreadTitles", |ctx: Arc<ConnCtx>, input: ThreadIds| async move { of_threads(&ctx.state, &input.thread_ids, |t| t.title) })
    .unary("orchestration.getThreadLinkMetadata", |ctx: Arc<ConnCtx>, input: ThreadIds| async move {
        of_threads(&ctx.state, &input.thread_ids, |t| LinkMetadata { title: t.title, project_id: t.project_id })
    })
    .unary("orchestration.getTurnItem", |ctx: Arc<ConnCtx>, input: TurnItemInput| async move {
        let item = turn_item(&ctx.state, &input.thread_id, &input.item_id).map_err(|_| Failure::projection(&input.thread_id, "Failed to load turn item"))?;
        Ok::<_, Failure>(TurnItemAnswer { item })
    })
    .unary("orchestration.getThreadFamily", |ctx: Arc<ConnCtx>, input: ThreadOnly| async move {
        get_thread_family(&ctx.state, &input.thread_id)
    })
    .unary("orchestration.getWorktreeThreadIds", |ctx: Arc<ConnCtx>, input: WorktreeInput| async move {
        let rows = shell_rows(&ctx.state, |t| t.deleted_at.is_none() && t.worktree_path.as_deref() == Some(input.worktree_path.as_str()))?;
        Ok::<_, Failure>(WorktreeThreadIds { thread_ids: rows.threads.into_iter().map(|shell| shell.id).collect() })
    })
    .unary("orchestration.getArchivedShellSnapshot", |ctx: Arc<ConnCtx>, _: serde::de::IgnoredAny| async move {
        let snap = super::shell_snapshot(ctx.state.clone()).await.map_err(Failure::shell)?;
        Ok::<_, Failure>(ArchivedShellSnapshot {
            schema_version: snap.schema_version,
            snapshot_sequence: snap.snapshot_sequence,
            projects: snap.projects,
            threads: snap.archived_threads,
        })
    })
    .unary("orchestration.searchThread", |ctx: Arc<ConnCtx>, input: FindInput| async move { search_thread(&ctx.state, &input) })
    .stream("orchestration.searchThreadStream", |ctx: Arc<ConnCtx>, input: FindInput| {
        futures::stream::once(async move { search_thread(&ctx.state, &input) }).boxed()
    })
    .unary("orchestration.searchThreads", |ctx: Arc<ConnCtx>, input: SearchThreadsInput| async move {
        search_threads(&ctx.state, &input)
    })
    .unary("orchestration.forceStopThread", force_stop)
    .unary("orchestration.stopWorkflow", stop_workflow)
    .unary("orchestration.getWorkflowScript", |_ctx: Arc<ConnCtx>, input: WorkflowScriptInput| async move {
        read_workflow_script(&input.script_path)
    })
    .unary("orchestration.getNativeChildWork", |_ctx: Arc<ConnCtx>, _input: ThreadOnly| async move {
        Ok::<_, Failure>(NativeChildWork { children: [] })
    })
    .stream("orchestration.subscribeCommandOutput", |ctx, input: CommandOutputInput| command_output(ctx, input))
    .stream("orchestration.subscribePromptSuggestion", |_ctx: Arc<ConnCtx>, _input: ThreadOnly| {
        // Domovoi's adapters produce no prompt suggestions; the latest is none.
        futures::stream::once(async { Ok::<Option<Nothing>, Failure>(None) }).chain(futures::stream::pending())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn docs(counts: &[i64]) -> Vec<Doc> {
        counts.iter().enumerate().map(|(i, c)| Doc { entry_id: format!("m{i}"), run_id: None, count: *c }).collect()
    }

    fn input(index: Option<i64>, offset: Option<i64>, start: Option<(&str, i64)>) -> FindInput {
        FindInput {
            thread_id: "t".into(),
            query: "q".into(),
            index,
            offset,
            start: start.map(|(e, o)| FindStart { entry_id: e.into(), occurrence: o }),
        }
    }

    #[test]
    fn find_wraps_offsets_and_resumes_from_start() {
        let d = docs(&[2, 0, 3]);
        let r = find_result(&d, &input(None, Some(-1), None), 7);
        assert_eq!((r.total_matches, r.active_index), (5, 4));
        assert_eq!(json!(r.found), json!({ "entryId": "m2", "runId": null, "occurrence": 2 }));
        let r = find_result(&d, &input(None, Some(1), Some(("m2", 1))), 7);
        assert_eq!(r.active_index, 4);
        assert_eq!(r.navigation.len(), 2, "entries without matches are left out");
        let r = find_result(&d, &input(Some(9), None, None), 7);
        assert_eq!(r.active_index, 4, "an explicit index is clamped, not wrapped");
        let r = find_result(&docs(&[]), &input(None, None, None), 7);
        // As the client is sent it.
        assert_eq!(json!(r), json!({ "complete": true, "snapshotSequence": 7, "totalMatches": 0, "activeIndex": 0, "navigation": [], "match": null }));
    }

    #[test]
    fn find_navigation_is_a_window_of_17_around_the_match() {
        let d = docs(&[1; 30]);
        let r = find_result(&d, &input(Some(0), None, None), 1);
        assert_eq!(r.navigation.len(), 17);
        assert_eq!(r.navigation[8].entry_id, "m0");
        assert_eq!(r.navigation[0].entry_id, "m22");
        assert_eq!(json!(r.navigation[8]), json!({ "entryId": "m0", "runId": null, "startIndex": 0, "count": 1 }));
    }

    #[test]
    fn occurrence_folding_matches_t3code() {
        assert_eq!(occurrences("Don’t DON'T don't", "don't").len(), 3);
        assert_eq!(occurrences("aaaa", "aa").len(), 2, "non-overlapping");
        assert_eq!(occurrences("ΟΔΟΣ", "οδοσ").len(), 1);
        assert!(occurrences("", "x").is_empty() && occurrences("x", "").is_empty());
    }

    #[test]
    fn snippet_leads_with_a_word_boundary() {
        let text = format!("{} needle and **more** `code`", "word ".repeat(20));
        let s = snippet(&text, text.find("needle").unwrap());
        assert!(s.starts_with('…') && s.contains("needle and more code"), "{s}");
        assert_eq!(snippet("short needle", 6), "short needle");
    }

    #[test]
    fn answers_are_written_as_the_client_reads_them() {
        let stopped = ForceStopped { outcome: ForceStopOutcome::Released, shared_with_thread_ids: [] };
        assert_eq!(json!(stopped), json!({ "outcome": "released", "sharedWithThreadIds": [] }));
        assert_eq!(json!(NativeChildWork { children: [] }), json!({ "children": [] }));
        assert_eq!(json!(None::<Nothing>), json!(null));
        let frame = OutputFrame { text: "a".into(), truncated: false, running: true };
        assert_eq!(json!(frame), json!({ "kind": "replace", "text": "a", "truncated": false, "running": true }));
        let found = ThreadsFound { matches: vec![], snapshot_sequence: None, threads: None };
        assert_eq!(json!(found), json!({ "matches": [] }));
    }
}
