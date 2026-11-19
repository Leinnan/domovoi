//! Scheduled tasks, after t3code's `ScheduledTaskService` and `Schedule.ts`:
//! stored in SQLite, listed live, fired by a timer task through the
//! orchestrator (a new thread per run, or a message to the bound thread).
//!
//! Differences from t3code:
//! - webhook deliveries (`crate::webhooks`) have no T3 Connect relay;
//! - no managed policy (`blockedByPolicy`, runtime-mode ceilings), no scratch
//!   projects and no agent secret requests (`secretRef` is refused).
//!
//! A fresh-session run sends `freshContext`: the occurrence starts a provider
//! session of its own, a side conversation the thread's own continues past.
//!
//! A task is the record clients read ([`ScheduledTask`]). Its row holds the
//! schedule, the workspace strategy and the model selection as the JSON those
//! records encode to, which is also what the server wrote before a task was a
//! record: rows of then are read as they were sent.

#![expect(clippy::disallowed_types, reason = "request fields read leniently inside their readers; a schedule's `type` read before its record is chosen")]

use crate::state::SharedState;
use crate::store::Store;
use jiff::{SignedDuration, Timestamp, ToSpan, Zoned, tz::TimeZone};
use rusqlite::{OptionalExtension, Row, params};
use serde::de::{DeserializeOwned, Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use domovoi_contracts::time_fmt::iso_from_millis;
use domovoi_model::typed::command::MessageDispatch;
use domovoi_model::typed::{
    Actor, CreationSource, FixedTimeSchedule, InitialMessage, InteractionMode, ModelSelection, OnceSchedule, Run, RunStatus, RuntimeMode, ScheduledTask,
    ScheduledTaskList, ScheduledTaskRunStatus, ScheduledTaskSchedule, ScheduledTaskThreadMode, ScheduledTaskWebhookEndpoint, ThreadLaunchInput, TurnItem,
    WebhookDedupe, WebhookFilter, WebhookSchedule, WebhookSignature, WebhookSignatureEncoding, WorkspaceStrategy,
};
use tokio::sync::{Notify, broadcast};

/// A time as a task's record holds it: the ISO string clients read.
type WireTime = domovoi_model::typed::Timestamp;

pub const MIN_INTERVAL_MS: i64 = 60_000;
const MISSED_FIXED_TIME_GRACE_MS: i64 = 10 * 60_000;
const RUN_OUTCOME_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
/// The timer re-checks at least this often (clock changes, sleep/wake).
const MAX_IDLE_WAIT: Duration = Duration::from_secs(60);
const WEBHOOK_ROUTE_PREFIX: &str = "/api/hooks";

// ---- schedule computation (Schedule.ts) ----

fn parse_time_of_day(value: &str) -> Option<(i8, i8)> {
    let (h, m) = value.trim().split_once(':')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || h.len() > 2 || !digits(m) || m.len() != 2 {
        return None;
    }
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 23 && m <= 59).then_some((h, m))
}

/// The moment a one-time schedule names, when its `runAt` is a time.
fn run_at(once: &OnceSchedule) -> Option<Timestamp> {
    once.run_at.as_str().trim().parse().ok()
}

/// `nextScheduledRunAt`: the first occurrence strictly after `from`.
/// Fixed times are wall-clock times in `from`'s zone.
pub fn next_scheduled_run(schedule: &ScheduledTaskSchedule, from: &Zoned) -> Option<Timestamp> {
    match schedule {
        ScheduledTaskSchedule::Once(once) => run_at(once).filter(|at| *at > from.timestamp()),
        // Intervals advance elapsed time, including across daylight-saving changes.
        ScheduledTaskSchedule::Interval(interval) => {
            let every = i64::try_from(interval.every_ms).ok()?.max(MIN_INTERVAL_MS);
            from.timestamp().checked_add(SignedDuration::from_millis(every)).ok()
        }
        ScheduledTaskSchedule::FixedTime(fixed) => {
            let (hour, minute) = parse_time_of_day(&fixed.time_of_day)?;
            let weekdays = fixed.weekdays.as_deref().unwrap_or_default();
            for offset in 0..=7 {
                let date = from.date().checked_add(offset.days()).ok()?;
                let candidate = date.at(hour, minute, 0, 0).to_zoned(from.time_zone().clone()).ok()?;
                if candidate.timestamp() <= from.timestamp() {
                    continue;
                }
                if !weekdays.is_empty() && !weekdays.contains(&u64::from(candidate.weekday().to_sunday_zero_offset().unsigned_abs())) {
                    continue;
                }
                return Some(candidate.timestamp());
            }
            None
        }
        ScheduledTaskSchedule::Webhook(_) | ScheduledTaskSchedule::Other(_) => None,
    }
}

fn weekday_key(fixed: &FixedTimeSchedule) -> String {
    let mut days = fixed.weekdays.clone().unwrap_or_default();
    days.sort_unstable();
    days.dedup();
    if days.is_empty() || days.len() == 7 {
        return "daily".into();
    }
    days.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// `isSameSchedule`: whether an edit leaves the pending run where it is.
pub fn is_same_schedule(a: &ScheduledTaskSchedule, b: &ScheduledTaskSchedule) -> bool {
    use ScheduledTaskSchedule::{FixedTime, Interval, Once, Webhook};
    match (a, b) {
        (Interval(a), Interval(b)) => a.every_ms == b.every_ms,
        (Webhook(_), Webhook(_)) => true,
        (Once(a), Once(b)) => matches!((run_at(a), run_at(b)), (Some(x), Some(y)) if x == y),
        (FixedTime(a), FixedTime(b)) => {
            let time = |fixed: &FixedTimeSchedule| parse_time_of_day(&fixed.time_of_day);
            matches!((time(a), time(b)), (Some(x), Some(y)) if x == y) && weekday_key(a) == weekday_key(b)
        }
        _ => false,
    }
}

/// `isMissedFixedTimeRun`: a fixed-time run more than ten minutes late is skipped.
pub fn is_missed_fixed_time_run(schedule: &ScheduledTaskSchedule, due: Timestamp, now: Timestamp) -> bool {
    matches!(schedule, ScheduledTaskSchedule::FixedTime(_)) && now.as_millisecond() - due.as_millisecond() > MISSED_FIXED_TIME_GRACE_MS
}

fn iso(ts: Timestamp) -> String {
    iso_from_millis(ts.as_millisecond())
}

/// A time of a task's record as the moment it names.
fn moment(time: Option<&WireTime>) -> Option<Timestamp> {
    time?.as_str().parse().ok()
}

/// `nextRunAt`: none while disabled.
fn next_run_at(enabled: bool, schedule: &ScheduledTaskSchedule, from: &Zoned) -> Option<String> {
    if !enabled {
        return None;
    }
    next_scheduled_run(schedule, from).map(iso)
}

/// `nextRunAfterRun`: a one-time task counts from its run's start.
fn next_run_after_run(task: &ScheduledTask, now: &Zoned) -> Option<String> {
    let started = matches!(task.schedule, ScheduledTaskSchedule::Once(_)).then(|| moment(task.last_run_at.as_ref())).flatten();
    let from = started.map(|t| t.to_zoned(now.time_zone().clone())).unwrap_or_else(|| now.clone());
    next_run_at(task.enabled, &task.schedule, &from)
}

fn local_now() -> Zoned {
    Zoned::now()
}

/// The server's IANA zone, which fixed times are read in.
pub fn time_zone_name() -> Option<String> {
    TimeZone::system().iana_name().map(str::to_owned)
}

// ---- failures ----

/// Why a call of the scheduled-task group failed, as the client is told
/// (t3code's `ScheduledTaskError`): what to show, the task it was about when
/// there was one, and what the storage said when it was the storage that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{message}")]
#[serde(tag = "_tag", rename = "ScheduledTaskError", rename_all = "camelCase")]
pub struct ScheduledTaskError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl ScheduledTaskError {
    /// The failure as the client gets it, for the log (its cause included).
    fn written(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.message.clone())
    }
}

pub fn task_error(message: &str, task_id: Option<&str>) -> ScheduledTaskError {
    ScheduledTaskError { message: message.to_owned(), task_id: task_id.map(str::to_owned), cause: None }
}

pub(crate) fn task_error_cause(message: &str, task_id: Option<&str>, cause: impl std::fmt::Display) -> ScheduledTaskError {
    ScheduledTaskError { cause: Some(cause.to_string()), ..task_error(message, task_id) }
}

// ---- what an upsert is sent ----

/// Reads a field as the handler did before its payload was a record: a value
/// that is not of the field's type counts as not sent.
fn lenient<'de, T: DeserializeOwned, D: Deserializer<'de>>(sent: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(sent)?).ok())
}

/// Reads a field that may be left out or sent as `null`: `Some(None)` for
/// `null`, so a field sent empty is told from one not sent.
fn sent_or_null<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(sent).map(Some)
}

/// What `scheduledTasks.upsert` is sent (`ScheduledTaskUpsertInput`), and
/// what the `schedule_task` tool makes of its arguments.
///
/// The schedule and the model selection must be what the schema says; a
/// payload where one is not is not decoded. The other fields are read as
/// leniently as before this was a record: one left out, or of another type,
/// takes its default, or is refused by the table when it has none.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertInput {
    #[serde(default, deserialize_with = "lenient")]
    pub id: Option<String>,
    /// Without an id, the task's id is made from this.
    #[serde(default, deserialize_with = "lenient")]
    pub command_id: Option<String>,
    /// Refuse the save when the task is no longer there (an edit from a form).
    #[serde(default, deserialize_with = "lenient")]
    pub require_existing: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub enabled: Option<bool>,
    pub schedule: UpsertSchedule,
    #[serde(default, deserialize_with = "lenient")]
    pub project_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub thread_id: Option<String>,
    /// Left out: the stored mode, or the one a thread id implies.
    #[serde(default, deserialize_with = "lenient")]
    pub thread_mode: Option<ScheduledTaskThreadMode>,
    /// A task saved without one works in the project's folder.
    #[serde(default)]
    pub workspace_strategy: Option<WorkspaceStrategy>,
    pub model_selection: ModelSelection,
    #[serde(default, deserialize_with = "lenient")]
    pub runtime_mode: Option<RuntimeMode>,
    #[serde(default, deserialize_with = "lenient")]
    pub interaction_mode: Option<InteractionMode>,
    #[serde(default, deserialize_with = "lenient")]
    pub created_by: Option<Actor>,
    #[serde(default, deserialize_with = "lenient")]
    pub creation_source: Option<CreationSource>,
}

/// The schedule an upsert is sent (`ScheduledTaskUpsertSchedule`): a schedule
/// as a task holds it, except that a webhook's comes with its secret and may
/// leave out what the stored one keeps.
#[derive(Debug, Clone)]
pub enum UpsertSchedule {
    Webhook(WebhookUpsert),
    /// Every other kind, a kind this build does not know among them.
    Other(ScheduledTaskSchedule),
}

impl<'de> Deserialize<'de> for UpsertSchedule {
    fn deserialize<D: Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        // Read into a JSON map once: the type may follow the fields it selects.
        let fields = serde_json::Map::<String, serde_json::Value>::deserialize(sent)?;
        let webhook = fields.get("type").and_then(serde_json::Value::as_str) == Some("webhook");
        let fields = serde_json::Value::Object(fields);
        if webhook {
            WebhookUpsert::deserialize(fields).map(Self::Webhook).map_err(D::Error::custom)
        } else {
            ScheduledTaskSchedule::deserialize(fields).map(Self::Other).map_err(D::Error::custom)
        }
    }
}

/// A webhook schedule as an upsert sends it (`ScheduledTaskUpsertWebhookSchedule`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookUpsert {
    /// Left out or `null`: the URL token alone admits requests.
    #[serde(default)]
    signature: Option<WebhookSignatureUpsert>,
    #[serde(default)]
    max_delivery_age_minutes: Option<u64>,
    /// Left out keeps the stored templates.
    #[serde(default, deserialize_with = "sent_or_null")]
    dedupe: Option<Option<WebhookDedupe>>,
    /// Left out keeps the stored filters; an empty list clears them.
    #[serde(default)]
    filters: Option<Vec<WebhookFilter>>,
}

/// The signature check of a webhook upsert, with the secret it is made with.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebhookSignatureUpsert {
    #[serde(default, deserialize_with = "lenient")]
    header: Option<String>,
    encoding: WebhookSignatureEncoding,
    prefix: String,
    /// Left out: the stored secret.
    #[serde(default, deserialize_with = "lenient")]
    secret: Option<String>,
    /// A secret the user entered for an agent, which Domovoi has no way to ask for.
    #[serde(default)]
    secret_ref: Option<IgnoredAny>,
}

// ---- storage ----

fn webhook_path(task_id: &str, token: &str) -> String {
    format!("{WEBHOOK_ROUTE_PREFIX}/{}/{token}", crate::util::encode_uri_component(task_id))
}

const COLUMNS: &str = "task_id, title, prompt, enabled, schedule_json, project_id, thread_id, thread_mode, \
    workspace_strategy_json, model_selection_json, runtime_mode, interaction_mode, created_by, creation_source, \
    created_at, updated_at, next_run_at, last_run_at, last_run_status, last_run_error, run_count, webhook_token, webhook_secret";

/// A task as its row holds it.
#[derive(Debug, Clone)]
struct StoredTask {
    task: ScheduledTask,
    /// `schedule_json` as the row spells it: the save of an edit that keeps
    /// the schedule is told this spelling, the one the row is compared by.
    schedule_json: String,
}

/// A JSON column as the record it holds.
fn json_column<T: DeserializeOwned>(r: &Row<'_>, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(&r.get::<_, String>(column)?)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e)))
}

fn task_from_row(r: &Row<'_>) -> rusqlite::Result<StoredTask> {
    let text = |column: usize| r.get::<_, String>(column);
    let time = |column: usize| r.get::<_, Option<String>>(column).map(|time| time.map(WireTime::from));
    let id = text(0)?;
    let schedule_json = text(4)?;
    let schedule: ScheduledTaskSchedule = json_column(r, 4)?;
    let token: Option<String> = r.get(21)?;
    let secret: Option<String> = r.get(22)?;
    let webhook = match (&schedule, token) {
        (ScheduledTaskSchedule::Webhook(_), Some(token)) => {
            let path = webhook_path(&id, &token);
            Some(ScheduledTaskWebhookEndpoint { url: crate::webhooks::public_url(&path), path, has_secret: secret.is_some() })
        }
        _ => None,
    };
    let task = ScheduledTask {
        title: text(1)?,
        prompt: text(2)?,
        enabled: r.get::<_, i64>(3)? == 1,
        schedule,
        project_id: text(5)?.into(),
        thread_id: r.get::<_, Option<String>>(6)?.map(Into::into),
        thread_mode: Some(text(7)?.as_str().into()),
        // A task saved without one works in the project's folder.
        workspace_strategy: json_column::<Option<WorkspaceStrategy>>(r, 8)?.unwrap_or(WorkspaceStrategy::Root { branch: None }),
        model_selection: json_column(r, 9)?,
        runtime_mode: text(10)?.as_str().into(),
        interaction_mode: text(11)?.as_str().into(),
        created_by: text(12)?.as_str().into(),
        creation_source: text(13)?.as_str().into(),
        created_at: text(14)?.into(),
        updated_at: text(15)?.into(),
        next_run_at: time(16)?,
        last_run_at: time(17)?,
        last_run_status: text(18)?.as_str().into(),
        last_run_error: r.get(19)?,
        run_count: u64::try_from(r.get::<_, i64>(20)?).unwrap_or(0),
        webhook,
        blocked_by_policy: None,
        id: id.into(),
    };
    Ok(StoredTask { task, schedule_json })
}

fn find_stored(store: &Store, id: &str) -> Result<Option<StoredTask>, ScheduledTaskError> {
    store
        .db()
        .with(|c| c.query_row(&format!("SELECT {COLUMNS} FROM scheduled_tasks WHERE task_id = ?1"), params![id], task_from_row).optional())
        .map_err(|e| task_error_cause("Could not load schedule task.", Some(id), e))
}

pub fn find(store: &Store, id: &str) -> Result<Option<ScheduledTask>, ScheduledTaskError> {
    Ok(find_stored(store, id)?.map(|stored| stored.task))
}

fn load(store: &Store, id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
    find(store, id)?.ok_or_else(|| task_error("Schedule task not found.", Some(id)))
}

/// A row of a listing: the task's id, and the task when its row can be read.
type ListedRow = (String, rusqlite::Result<StoredTask>);

fn listed_row(r: &Row<'_>) -> rusqlite::Result<ListedRow> {
    Ok((r.get(0)?, task_from_row(r)))
}

/// The tasks of a listing. A row that cannot be read (a schedule or a model
/// selection that is not one) is left out and logged, as a client leaves out
/// a task it cannot read: one such row does not hide the others, nor stop
/// the timer.
fn readable(rows: Vec<ListedRow>) -> Vec<ScheduledTask> {
    let mut tasks = Vec::with_capacity(rows.len());
    for (id, row) in rows {
        match row {
            Ok(stored) => tasks.push(stored.task),
            Err(error) => tracing::warn!(task = %id, %error, "a scheduled task's row cannot be read; it is left out"),
        }
    }
    tasks
}

/// Oldest-created last; toggles and runs must not reorder rows under the pointer.
pub fn list(store: &Store) -> Result<Vec<ScheduledTask>, ScheduledTaskError> {
    store
        .db()
        .with(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM scheduled_tasks ORDER BY created_at DESC, task_id ASC"))?;
            let rows = st.query_map([], listed_row)?;
            rows.collect()
        })
        .map(readable)
        .map_err(|e| task_error_cause("Could not list schedule tasks.", None, e))
}

pub fn list_result(store: &Store) -> Result<ScheduledTaskList, ScheduledTaskError> {
    Ok(ScheduledTaskList { tasks: list(store)?.into(), time_zone: Some(time_zone_name()) })
}

pub(crate) fn webhook_credentials(store: &Store, id: &str) -> Result<Option<(Option<String>, Option<String>)>, ScheduledTaskError> {
    store
        .db()
        .with(|c| {
            c.query_row("SELECT webhook_token, webhook_secret FROM scheduled_tasks WHERE task_id = ?1", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
        })
        .map_err(|e| task_error_cause("Could not load schedule task.", Some(id), e))
}

fn new_webhook_token() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(crate::util::random_bytes::<32>())
}

struct WebhookSave {
    token: Option<String>,
    secret: Option<String>,
    secret_changed: bool,
}

/// What an upsert writes to a task's row. The table refuses a row without a
/// title, a prompt, a runtime mode or an interaction mode, which is how a
/// payload that leaves one out is turned down.
struct Draft {
    id: String,
    title: Option<String>,
    prompt: Option<String>,
    enabled: bool,
    schedule: ScheduledTaskSchedule,
    project_id: String,
    thread_id: Option<String>,
    thread_mode: ScheduledTaskThreadMode,
    workspace_strategy: Option<WorkspaceStrategy>,
    model_selection: ModelSelection,
    runtime_mode: Option<RuntimeMode>,
    interaction_mode: Option<InteractionMode>,
    created_by: Actor,
    creation_source: CreationSource,
    created_at: WireTime,
    updated_at: WireTime,
    next_run_at: Option<WireTime>,
    last_run_at: Option<WireTime>,
    last_run_status: ScheduledTaskRunStatus,
    last_run_error: Option<String>,
    run_count: u64,
}

impl Draft {
    /// The task as saved, for a save whose row is gone again before it is
    /// read back; `None` for a draft the table would have refused.
    fn into_task(self) -> Option<ScheduledTask> {
        Some(ScheduledTask {
            id: self.id.into(),
            title: self.title?,
            prompt: self.prompt?,
            enabled: self.enabled,
            schedule: self.schedule,
            project_id: self.project_id.into(),
            thread_id: self.thread_id.map(Into::into),
            thread_mode: Some(self.thread_mode),
            workspace_strategy: self.workspace_strategy.unwrap_or(WorkspaceStrategy::Root { branch: None }),
            model_selection: self.model_selection,
            runtime_mode: self.runtime_mode?,
            interaction_mode: self.interaction_mode?,
            created_by: self.created_by,
            creation_source: self.creation_source,
            created_at: self.created_at,
            updated_at: self.updated_at,
            next_run_at: self.next_run_at,
            last_run_at: self.last_run_at,
            last_run_status: self.last_run_status,
            last_run_error: self.last_run_error,
            run_count: self.run_count,
            webhook: None,
            blocked_by_policy: None,
        })
    }
}

/// A record as the JSON text a column holds.
fn json_text(record: &impl Serialize) -> rusqlite::Result<String> {
    serde_json::to_string(record).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// `saveTask`: settings saves never touch run-state columns, and an edit that
/// keeps the schedule keeps a `next_run_at` a finishing run moved meanwhile.
/// `kept_schedule` is the stored schedule's JSON when the edit keeps it.
fn save(store: &Store, task: &Draft, require_existing: bool, kept_schedule: Option<&str>, webhook: &WebhookSave) -> Result<(), ScheduledTaskError> {
    let id = task.id.as_str();
    let saved = store
        .db()
        .with(|c| {
            c.query_row(
                "INSERT INTO scheduled_tasks (task_id, title, prompt, enabled, schedule_json, project_id, thread_id, thread_mode,
                    workspace_strategy_json, model_selection_json, runtime_mode, interaction_mode, created_by, creation_source,
                    created_at, updated_at, next_run_at, last_run_at, last_run_status, last_run_error, run_count,
                    webhook_token, webhook_secret)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
                 WHERE ?24 = 0 OR EXISTS (SELECT 1 FROM scheduled_tasks WHERE task_id = ?1)
                 ON CONFLICT (task_id) DO UPDATE SET
                    title = excluded.title, prompt = excluded.prompt, enabled = excluded.enabled,
                    schedule_json = excluded.schedule_json, project_id = excluded.project_id,
                    thread_id = CASE
                      WHEN excluded.thread_mode = 'new_thread' THEN NULL
                      WHEN excluded.project_id != scheduled_tasks.project_id THEN excluded.thread_id
                      ELSE COALESCE(excluded.thread_id, scheduled_tasks.thread_id) END,
                    thread_mode = excluded.thread_mode,
                    workspace_strategy_json = excluded.workspace_strategy_json,
                    model_selection_json = excluded.model_selection_json,
                    runtime_mode = excluded.runtime_mode, interaction_mode = excluded.interaction_mode,
                    creation_source = excluded.creation_source, updated_at = excluded.updated_at,
                    next_run_at = CASE
                      WHEN ?25 IS NOT NULL AND scheduled_tasks.schedule_json IN (excluded.schedule_json, ?25)
                        AND scheduled_tasks.enabled = excluded.enabled
                      THEN scheduled_tasks.next_run_at ELSE excluded.next_run_at END,
                    webhook_token = CASE
                      WHEN excluded.webhook_token IS NULL THEN NULL
                      ELSE COALESCE(scheduled_tasks.webhook_token, excluded.webhook_token) END,
                    webhook_secret = CASE WHEN ?26 = 1 THEN excluded.webhook_secret ELSE scheduled_tasks.webhook_secret END
                 RETURNING task_id",
                params![
                    id,
                    task.title,
                    task.prompt,
                    task.enabled,
                    json_text(&task.schedule)?,
                    task.project_id,
                    task.thread_id,
                    task.thread_mode.as_str(),
                    json_text(&task.workspace_strategy)?,
                    json_text(&task.model_selection)?,
                    task.runtime_mode.as_ref().map(RuntimeMode::as_str),
                    task.interaction_mode.as_ref().map(InteractionMode::as_str),
                    task.created_by.as_str(),
                    task.creation_source.as_str(),
                    task.created_at.as_str(),
                    task.updated_at.as_str(),
                    task.next_run_at.as_ref().map(WireTime::as_str),
                    task.last_run_at.as_ref().map(WireTime::as_str),
                    task.last_run_status.as_str(),
                    task.last_run_error,
                    i64::try_from(task.run_count).unwrap_or(i64::MAX),
                    webhook.token,
                    webhook.secret,
                    require_existing,
                    kept_schedule,
                    webhook.secret_changed,
                ],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .map_err(|e| task_error_cause("Could not save schedule task.", Some(id), e))?;
    saved.map(|_| ()).ok_or_else(|| task_error("Schedule task not found.", Some(id)))
}

/// `resolveThreadMode`: clients that predate thread modes send none.
fn resolve_thread_mode(sent: Option<ScheduledTaskThreadMode>, thread_sent: bool, existing: Option<&ScheduledTask>) -> ScheduledTaskThreadMode {
    if let Some(mode) = sent {
        return mode;
    }
    let stored = existing.map(thread_mode).unwrap_or(ScheduledTaskThreadMode::NewThread);
    if stored == ScheduledTaskThreadMode::NewThread && thread_sent { ScheduledTaskThreadMode::Continue } else { stored }
}

/// The launch of the thread an occurrence of `task` starts.
fn occurrence_launch(task: &ScheduledTask, command_id: &str, message_id: &str) -> ThreadLaunchInput {
    ThreadLaunchInput {
        command_id: command_id.into(),
        creation_source: Some(task.creation_source.clone()),
        thread_id: None,
        reuse_existing_thread: None,
        project_id: task.project_id.clone(),
        title: task.title.clone(),
        generate_title: None,
        model_selection: task.model_selection.clone(),
        runtime_mode: task.runtime_mode.clone(),
        interaction_mode: task.interaction_mode.clone(),
        workspace_strategy: task.workspace_strategy.clone(),
        initial_message: Some(InitialMessage { message_id: Some(message_id.into()), text: task.prompt.clone(), context: None, attachments: vec![] }),
        created_by: Some(task.created_by.clone()),
        delegated_from: None,
    }
}

/// The message an occurrence of `task` sends to the thread it is bound to.
fn occurrence_message(task: &ScheduledTask, command_id: &str, thread_id: &str, message_id: &str) -> MessageDispatch {
    MessageDispatch::new(command_id, thread_id, message_id, task.prompt.clone(), task.created_by.clone(), task.creation_source.clone())
}

/// `scheduledTaskThreadMode`.
fn thread_mode(task: &ScheduledTask) -> ScheduledTaskThreadMode {
    let bound = if task.thread_id.is_none() { ScheduledTaskThreadMode::NewThread } else { ScheduledTaskThreadMode::Continue };
    task.thread_mode.clone().unwrap_or(bound)
}

fn require_destination_thread(store: &Store, thread_id: &str, project_id: &str, task_id: &str) -> Result<(), ScheduledTaskError> {
    let thread = store
        .app_thread(thread_id)
        .map_err(|e| task_error_cause("Could not read the destination thread.", Some(task_id), e))?;
    let Some(thread) = thread.filter(|t| t.deleted_at.is_none()) else {
        return Err(task_error("That thread no longer exists.", Some(task_id)));
    };
    if thread.archived_at.is_some() {
        return Err(task_error("That thread is archived. Unarchive it or pick another destination.", Some(task_id)));
    }
    if thread.project_id.as_str() != project_id {
        return Err(task_error("That thread belongs to a different project.", Some(task_id)));
    }
    Ok(())
}

/// A webhook schedule as stored: the header lower-cased, the secret left
/// out, and what the upsert did not send taken from the stored schedule.
fn normalize_webhook(sent: WebhookUpsert, prior: Option<&WebhookSchedule>) -> WebhookSchedule {
    let signature =
        sent.signature.map(|s| WebhookSignature { header: s.header.unwrap_or_default().to_lowercase(), encoding: s.encoding, prefix: s.prefix });
    let dedupe = match sent.dedupe {
        Some(dedupe) => dedupe,
        None => prior.and_then(|p| p.dedupe.clone()),
    };
    // Omitted keeps the stored filters.
    let filters = sent.filters.or_else(|| prior.and_then(|p| p.filters.clone())).filter(|f| !f.is_empty());
    WebhookSchedule { signature, max_delivery_age_minutes: Some(sent.max_delivery_age_minutes), dedupe, filters }
}

/// A one-time schedule as stored: its run in UTC (a `runAt` that is no time stays as sent).
fn normalize_once(sent: OnceSchedule) -> OnceSchedule {
    match run_at(&sent) {
        Some(at) => OnceSchedule { run_at: iso(at).into() },
        None => sent,
    }
}

/// `upsert`.
pub fn upsert(store: &Store, input: UpsertInput, now: &Zoned) -> Result<ScheduledTask, ScheduledTaskError> {
    let id = match (input.id, input.command_id) {
        (Some(id), _) => id,
        (None, Some(c)) => format!("scheduled-task:{c}"),
        (None, None) => format!("scheduled-task:{}", crate::util::new_uuid()),
    };
    let existing = find_stored(store, &id)?;
    let prior = existing.as_ref().map(|stored| &stored.task);
    let prior_thread = prior.and_then(|t| t.thread_id.as_ref()).map(|thread| thread.as_str());
    let mode = resolve_thread_mode(input.thread_mode, input.thread_id.is_some(), prior);
    let project_id = input.project_id.unwrap_or_default();
    let kept_thread = input.thread_id.or_else(|| prior_thread.map(str::to_owned));
    // A bound thread belongs to its project, so moving the task leaves it for a new one.
    let moved_off_thread = prior.is_some_and(|t| t.project_id.as_str() != project_id && kept_thread.as_deref() == prior_thread);
    let thread_id = if mode == ScheduledTaskThreadMode::NewThread || moved_off_thread { None } else { kept_thread };
    if let Some(tid) = &thread_id {
        if prior_thread != Some(tid.as_str()) {
            require_destination_thread(store, tid, &project_id, &id)?;
        }
    }
    let enabled = input.enabled == Some(true);
    let (schedule, webhook) = match input.schedule {
        UpsertSchedule::Webhook(sent) => {
            let creds = if existing.is_some() { webhook_credentials(store, &id)? } else { None };
            let (old_token, old_secret) = creds.unwrap_or((None, None));
            if sent.signature.as_ref().is_some_and(|s| s.secret_ref.is_some()) {
                return Err(task_error("That secret request is no longer available. Ask for the secret again.", Some(&id)));
            }
            let provided = sent.signature.as_ref().and_then(|s| s.secret.clone());
            let has_signature = sent.signature.is_some();
            let secret = if has_signature { provided.clone().or(old_secret) } else { None };
            if has_signature && secret.is_none() {
                return Err(task_error("A webhook signature check needs a signing secret.", Some(&id)));
            }
            let save = WebhookSave {
                token: Some(old_token.unwrap_or_else(new_webhook_token)),
                secret,
                secret_changed: !has_signature || provided.is_some() || existing.is_none(),
            };
            let stored = prior.and_then(|t| match &t.schedule {
                ScheduledTaskSchedule::Webhook(stored) => Some(stored),
                _ => None,
            });
            (ScheduledTaskSchedule::Webhook(normalize_webhook(sent, stored)), save)
        }
        UpsertSchedule::Other(sent) => {
            let schedule = match sent {
                ScheduledTaskSchedule::Once(once) => ScheduledTaskSchedule::Once(normalize_once(once)),
                other => other,
            };
            (schedule, WebhookSave { token: None, secret: None, secret_changed: true })
        }
    };
    let unchanged = existing.as_ref().filter(|stored| stored.task.enabled == enabled && is_same_schedule(&stored.task.schedule, &schedule));
    if unchanged.is_none() && enabled && matches!(schedule, ScheduledTaskSchedule::Once(_)) && next_run_at(true, &schedule, now).is_none() {
        return Err(task_error("The run time has passed. Pick a time in the future.", Some(&id)));
    }
    let ts = WireTime::from(iso(now.timestamp()));
    let draft = Draft {
        title: input.title,
        prompt: input.prompt,
        enabled,
        next_run_at: match unchanged {
            Some(stored) => stored.task.next_run_at.clone(),
            None => next_run_at(enabled, &schedule, now).map(WireTime::from),
        },
        schedule,
        project_id,
        thread_id,
        thread_mode: mode,
        workspace_strategy: input.workspace_strategy,
        model_selection: input.model_selection,
        runtime_mode: input.runtime_mode,
        interaction_mode: input.interaction_mode,
        created_by: prior.map(|t| t.created_by.clone()).unwrap_or_else(|| input.created_by.unwrap_or(Actor::User)),
        creation_source: input.creation_source.unwrap_or(CreationSource::Web),
        created_at: prior.map_or_else(|| ts.clone(), |t| t.created_at.clone()),
        updated_at: ts,
        last_run_at: prior.and_then(|t| t.last_run_at.clone()),
        last_run_status: prior.map_or(ScheduledTaskRunStatus::Never, |t| t.last_run_status.clone()),
        last_run_error: prior.and_then(|t| t.last_run_error.clone()),
        run_count: prior.map_or(0, |t| t.run_count),
        id,
    };
    save(store, &draft, input.require_existing == Some(true), unchanged.map(|stored| stored.schedule_json.as_str()), &webhook)?;
    match find(store, &draft.id)? {
        Some(saved) => Ok(saved),
        None => {
            let id = draft.id.clone();
            draft.into_task().ok_or_else(|| task_error("Schedule task not found.", Some(&id)))
        }
    }
}

/// `setEnabled`.
pub fn set_enabled(store: &Store, id: &str, enabled: bool, now: &Zoned) -> Result<(ScheduledTask, bool), ScheduledTaskError> {
    let existing = load(store, id)?;
    if existing.enabled == enabled {
        return Ok((existing, false));
    }
    let next = next_run_at(enabled, &existing.schedule, now);
    if enabled && matches!(existing.schedule, ScheduledTaskSchedule::Once(_)) && next.is_none() {
        return Err(task_error("The run time has passed. Pick a time in the future.", Some(id)));
    }
    let ts = iso(now.timestamp());
    let updated = store
        .db()
        .with(|c| {
            c.query_row(
                "UPDATE scheduled_tasks SET enabled = ?2, next_run_at = ?3, updated_at = ?4 WHERE task_id = ?1 RETURNING task_id",
                params![id, enabled, next, ts],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .map_err(|e| task_error_cause("Could not update schedule task.", Some(id), e))?;
    if updated.is_none() {
        return Err(task_error("Schedule task not found.", Some(id)));
    }
    let task = ScheduledTask { enabled, next_run_at: next.map(WireTime::from), updated_at: ts.into(), ..existing };
    Ok((task, true))
}

pub fn delete(store: &Store, id: &str) -> Result<(), ScheduledTaskError> {
    store
        .db()
        .with(|c| c.execute("DELETE FROM scheduled_tasks WHERE task_id = ?1", params![id]).map(|_| ()))
        .map_err(|e| task_error_cause("Could not delete schedule task.", Some(id), e))
}

/// `rotateWebhookToken`.
pub fn rotate_webhook_token(store: &Store, id: &str, now: &Zoned) -> Result<ScheduledTask, ScheduledTaskError> {
    let task = load(store, id)?;
    if !matches!(task.schedule, ScheduledTaskSchedule::Webhook(_)) {
        return Err(task_error("Only webhook tasks have a URL token.", Some(id)));
    }
    let updated = store
        .db()
        .with(|c| {
            c.query_row(
                "UPDATE scheduled_tasks SET webhook_token = ?3, updated_at = ?4 WHERE task_id = ?1 AND created_at = ?2 RETURNING task_id",
                params![id, task.created_at.as_str(), new_webhook_token(), iso(now.timestamp())],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .map_err(|e| task_error_cause("Could not rotate webhook token.", Some(id), e))?;
    if updated.is_none() {
        return Err(task_error("Schedule task was deleted or replaced.", Some(id)));
    }
    load(store, id)
}

/// Enabled tasks whose run is due and not already running, soonest first.
///
/// A due row that cannot be read is taken off the timer: it can never run,
/// and left due it would have the timer look again at once, without end.
fn due_tasks(store: &Store, now: &Zoned) -> Result<Vec<ScheduledTask>, ScheduledTaskError> {
    let now_iso = iso(now.timestamp());
    let rows: Vec<ListedRow> = store
        .db()
        .with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {COLUMNS} FROM scheduled_tasks WHERE enabled = 1 AND next_run_at IS NOT NULL
                   AND next_run_at <= ?1 AND last_run_status <> 'running' ORDER BY next_run_at ASC, task_id ASC"
            ))?;
            let rows = st.query_map(params![now_iso], listed_row)?;
            rows.collect()
        })
        .map_err(|e| task_error_cause("Could not list schedule tasks.", None, e))?;
    for (id, _) in rows.iter().filter(|(_, row)| row.is_err()) {
        let _ = store.db().with(|c| c.execute("UPDATE scheduled_tasks SET next_run_at = NULL WHERE task_id = ?1", params![id]));
    }
    Ok(readable(rows))
}

fn earliest_next_run(store: &Store) -> Option<Timestamp> {
    let next: Option<String> = store
        .db()
        .with(|c| {
            c.query_row(
                "SELECT MIN(next_run_at) FROM scheduled_tasks WHERE enabled = 1 AND next_run_at IS NOT NULL AND last_run_status <> 'running'",
                [],
                |r| r.get(0),
            )
        })
        .ok()
        .flatten();
    next.and_then(|s| s.parse().ok())
}

/// Moves a skipped occurrence on; a stale snapshot (the row moved since) is left alone.
fn reschedule_missed(store: &Store, task: &ScheduledTask, now: &Zoned, run_error: Option<&str>) -> Result<bool, ScheduledTaskError> {
    let next = next_run_at(task.enabled, &task.schedule, now);
    let id = task.id.as_str();
    let n = store
        .db()
        .with(|c| {
            c.execute(
                "UPDATE scheduled_tasks SET next_run_at = ?2, updated_at = ?3,
                   last_run_status = CASE WHEN ?6 IS NULL THEN last_run_status ELSE 'failed' END,
                   last_run_error = CASE WHEN ?6 IS NULL THEN last_run_error ELSE ?6 END
                 WHERE task_id = ?1 AND next_run_at IS ?4 AND last_run_at IS ?5",
                params![id, next, iso(now.timestamp()), task.next_run_at.as_ref().map(WireTime::as_str), task.last_run_at.as_ref().map(WireTime::as_str), run_error],
            )
        })
        .map_err(|e| task_error_cause("Could not reschedule missed schedule task run.", Some(id), e))?;
    Ok(n > 0)
}

/// Rows a crash left `running` would never be picked up again.
fn recover_interrupted(store: &Store, now: &Zoned) -> usize {
    let Ok(tasks) = list(store) else { return 0 };
    let mut n = 0;
    for task in tasks.iter().filter(|t| t.last_run_status == ScheduledTaskRunStatus::Running) {
        let next = next_run_after_run(task, now);
        let _ = store.db().with(|c| {
            c.execute(
                "UPDATE scheduled_tasks SET last_run_status = 'failed', last_run_error = 'Run was interrupted by a server restart.',
                   next_run_at = ?2, updated_at = ?3, run_count = run_count + 1
                 WHERE task_id = ?1 AND last_run_status = 'running'",
                params![task.id.as_str(), next, iso(now.timestamp())],
            )
        });
        n += 1;
    }
    n
}

/// Whether a bound thread still holds an earlier occurrence in its queue (run
/// message ids carry the task id, so queued messages need not be read).
fn has_queued_occurrence(store: &Store, task_id: &str, thread_id: &str) -> bool {
    let prefix = format!("scheduled-task-message:{task_id}:");
    store.records::<Run>(thread_id).is_ok_and(|runs| runs.iter().any(|run| run.status == RunStatus::Queued && run.user_message_id.as_str().starts_with(&prefix)))
}

// ---- the service: change notifications, the timer, runs ----

pub struct ScheduledTasks {
    state: SharedState,
    changes: broadcast::Sender<()>,
    wake: Notify,
    /// Tasks with a run in progress (`Reservation`).
    active: Arc<Mutex<HashSet<String>>>,
}

/// What starts a run: the timer, or `runNow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    Scheduled,
    Manual,
    /// A webhook delivery (or a coalesced window of them).
    Webhook,
}

impl Trigger {
    /// As the ids of a run's command and message spell it.
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::Manual => "manual",
            Trigger::Webhook => "webhook",
        }
    }
}

/// What starts a run, and what it sends when that is not the task's prompt.
struct Occurrence {
    trigger: Trigger,
    prompt: Option<String>,
    /// Names the run's command and message; by default the start time and trigger.
    key: Option<String>,
}

/// A task's run in progress: while held, the task does not start again.
/// Dropped when the run ends, however it ends: a run that did not record
/// its outcome (a panic) is failed so the row does not stay `running`.
struct Reservation {
    active: Arc<Mutex<HashSet<String>>>,
    store: Store,
    task: ScheduledTask,
    recorded: bool,
}

impl Reservation {
    /// `None` when the task is already running.
    fn take(active: &Arc<Mutex<HashSet<String>>>, store: &Store, task: &ScheduledTask) -> Option<Self> {
        let id = task.id.as_str().to_owned();
        crate::util::locked(active).insert(id).then(|| Reservation { active: active.clone(), store: store.clone(), task: task.clone(), recorded: false })
    }

    fn id(&self) -> &str {
        self.task.id.as_str()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.recorded {
            let now = local_now();
            let _ = self.store.db().with(|c| {
                c.execute(
                    "UPDATE scheduled_tasks SET last_run_status = 'failed', last_run_error = 'Run stopped unexpectedly.',
                       next_run_at = ?2, updated_at = ?3, run_count = run_count + 1
                     WHERE task_id = ?1 AND last_run_status = 'running'",
                    params![self.id(), next_run_after_run(&self.task, &now), iso(now.timestamp())],
                )
            });
        }
        crate::util::locked(&self.active).remove(self.id());
    }
}

static SERVICE: OnceLock<Arc<ScheduledTasks>> = OnceLock::new();

/// The process-wide service; the first call recovers interrupted runs and
/// starts the timer (call it from inside the tokio runtime).
pub fn service(state: &SharedState) -> Arc<ScheduledTasks> {
    SERVICE
        .get_or_init(|| {
            let (changes, _) = broadcast::channel(16);
            let s = Arc::new(ScheduledTasks { state: state.clone(), changes, wake: Notify::new(), active: Arc::default() });
            recover_interrupted(&s.state.store, &local_now());
            let timer = s.clone();
            tokio::spawn(async move { timer.run_timer().await });
            s
        })
        .clone()
}

impl ScheduledTasks {
    pub fn store(&self) -> &Store {
        &self.state.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }

    /// Every write that can change the list or the next due time.
    pub fn notify_changed(&self) {
        let _ = self.changes.send(());
        self.wake.notify_one();
    }

    async fn run_timer(self: Arc<Self>) {
        crate::webhooks::recover(self.store());
        loop {
            crate::webhooks::run_closed_windows(&self);
            self.clone().run_due().await;
            let now = Timestamp::now();
            let next = earliest_next_run(self.store()).map(|next| next.as_millisecond());
            let window = crate::webhooks::next_window_close(self.store());
            let wait = next
                .into_iter()
                .chain(window)
                .min()
                .map(|next| Duration::from_millis(next.saturating_sub(now.as_millisecond()).max(0) as u64))
                .unwrap_or(MAX_IDLE_WAIT)
                .min(MAX_IDLE_WAIT);
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.wake.notified() => {}
            }
        }
    }

    /// `runDueTasks`: one at a time, oldest due first.
    async fn run_due(self: Arc<Self>) {
        let now = local_now();
        let Ok(due) = due_tasks(self.store(), &now) else { return };
        for task in due {
            let Some(due_at) = moment(task.next_run_at.as_ref()) else { continue };
            if is_missed_fixed_time_run(&task.schedule, due_at, now.timestamp()) {
                let msg = "Missed a scheduled run because the server was not running at that time.";
                if reschedule_missed(self.store(), &task, &now, Some(msg)).unwrap_or(false) {
                    self.notify_changed();
                }
                continue;
            }
            if let Err(e) = self.clone().run_task(&task, Trigger::Scheduled).await {
                tracing::warn!(task = task.id.as_str(), error = %e.written(), "scheduled task run failed");
            }
        }
    }

    /// `runNow`.
    pub async fn run_now(self: Arc<Self>, id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
        let task = load(self.store(), id)?;
        if matches!(task.schedule, ScheduledTaskSchedule::Webhook(_)) {
            return Err(task_error("Webhook tasks run when their URL receives a request.", Some(id)));
        }
        self.run_task(&task, Trigger::Manual).await
    }

    /// `runTask`: reserves the task, launches or sends, records the outcome.
    /// Returns the task as stored afterwards.
    ///
    /// The run is a task of its own: a caller that goes away (an RPC
    /// cancelled when its socket closes) does not stop it half way.
    async fn run_task(self: Arc<Self>, task: &ScheduledTask, trigger: Trigger) -> Result<ScheduledTask, ScheduledTaskError> {
        self.run_occurrence(task, Occurrence { trigger, prompt: None, key: None }).await
    }

    /// A webhook delivery's run: the task with the prompt its request filled
    /// in, keyed by the delivery so a retry of the dispatch is one run.
    pub(crate) async fn run_webhook(self: Arc<Self>, task: &ScheduledTask, prompt: String, delivery_id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
        let key = format!("{}:{delivery_id}", task.id.as_str());
        self.run_occurrence(task, Occurrence { trigger: Trigger::Webhook, prompt: Some(prompt), key: Some(key) }).await
    }

    async fn run_occurrence(self: Arc<Self>, task: &ScheduledTask, occurrence: Occurrence) -> Result<ScheduledTask, ScheduledTaskError> {
        let trigger = occurrence.trigger;
        let id = task.id.as_str().to_owned();
        let Some(reservation) = Reservation::take(&self.active, self.store(), task) else {
            return if trigger == Trigger::Scheduled { Ok(task.clone()) } else { Err(task_error("Schedule task is already running.", Some(&id))) };
        };
        let task = task.clone();
        let run = tokio::spawn(async move {
            // The whole guard moves in (not just the field set below), so it
            // lives as long as the run, not as long as the caller.
            let mut reservation = reservation;
            let result = self.run_reserved(&task, occurrence).await;
            // `run_reserved` records every outcome it returns.
            reservation.recorded = true;
            result
        });
        run.await.unwrap_or_else(|e| Err(task_error(&format!("Schedule task run stopped: {e}"), Some(&id))))
    }

    async fn run_reserved(self: Arc<Self>, task: &ScheduledTask, occurrence: Occurrence) -> Result<ScheduledTask, ScheduledTaskError> {
        let trigger = occurrence.trigger;
        let store = self.store().clone();
        let id = task.id.as_str().to_owned();
        let started = local_now();
        let started_iso = iso(started.timestamp());
        // Re-read: a stale poll snapshot must not fire a deleted, paused or postponed task.
        let Some(active) = find(&store, &id)? else {
            return if trigger == Trigger::Scheduled { Ok(task.clone()) } else { Err(task_error("Schedule task not found.", Some(&id))) };
        };
        if trigger == Trigger::Scheduled {
            let due = moment(active.next_run_at.as_ref());
            if !active.enabled || due.is_none_or(|d| d > started.timestamp()) {
                return Ok(active);
            }
            // A busy bound thread would otherwise collect one copy per interval.
            if !matches!(active.schedule, ScheduledTaskSchedule::Once(_)) {
                if let Some(tid) = active.thread_id.as_ref().map(|thread| thread.as_str()) {
                    if has_queued_occurrence(&store, &id, tid) {
                        if reschedule_missed(&store, &active, &started, None)? {
                            self.notify_changed();
                        }
                        return Ok(active);
                    }
                }
            }
        }
        let marked = store
            .db()
            .with(|c| {
                c.execute(
                    "UPDATE scheduled_tasks SET updated_at = ?2, last_run_at = ?2, last_run_status = 'running', last_run_error = NULL
                     WHERE task_id = ?1",
                    params![id, started_iso],
                )
            })
            .map_err(|e| task_error_cause("Could not mark schedule task as running.", Some(&id), e))?;
        if marked == 0 {
            return Err(task_error("Schedule task not found.", Some(&id)));
        }
        self.notify_changed();

        let fire_key = occurrence.key.clone().unwrap_or_else(|| format!("{id}:{}:{}", started.timestamp().as_millisecond(), trigger.as_str()));
        // A webhook's run says what its request filled in.
        let active = match &occurrence.prompt {
            Some(prompt) => ScheduledTask { prompt: prompt.clone(), ..active },
            None => active,
        };
        let command_id = format!("scheduled-task:{fire_key}");
        let message_id = format!("scheduled-task-message:{fire_key}");
        let mode = thread_mode(&active);
        let orchestrator = self.state.orchestrator.clone();
        // Why the occurrence did not start, as the task's last run shows it.
        let dispatched: Result<String, String> = match active.thread_id.as_ref().map(|thread| thread.as_str()) {
            None => {
                let launched = orchestrator.launch_thread(occurrence_launch(&active, &command_id, &message_id)).await;
                launched.map(|launched| launched.thread_id.as_str().to_owned()).map_err(|e| e.to_string()).inspect(|tid| {
                    if mode != ScheduledTaskThreadMode::NewThread {
                        bind_thread(&store, &active, tid);
                    }
                })
            }
            Some(tid) => {
                let mut send = occurrence_message(&active, &command_id, tid, &message_id);
                send.scheduled_task_id = Some(id.as_str().into());
                if mode == ScheduledTaskThreadMode::FreshSession {
                    send.fresh_context = Some(true);
                }
                orchestrator.dispatch(send.into()).await.map(|_| tid.to_owned()).map_err(|e| e.to_string())
            }
        };
        let completed = local_now();
        let (status, error, thread_id) = match &dispatched {
            Ok(tid) => (ScheduledTaskRunStatus::Succeeded, None, Some(tid.clone())),
            Err(e) => (ScheduledTaskRunStatus::Failed, Some(e.clone()), None),
        };
        // The stored row can have been edited meanwhile; compute the next run from it.
        let recorded = match find(&store, &id) {
            Ok(Some(current)) => {
                let from = if matches!(current.schedule, ScheduledTaskSchedule::Once(_)) { &started } else { &completed };
                let next = next_run_at(current.enabled, &current.schedule, from);
                let written = store
                    .db()
                    .with(|c| {
                        c.execute(
                            "UPDATE scheduled_tasks SET updated_at = ?2, next_run_at = ?3, last_run_status = ?4, last_run_error = ?5,
                               run_count = run_count + 1
                             WHERE task_id = ?1 AND last_run_status = 'running' AND last_run_at = ?6",
                            params![id, iso(completed.timestamp()), next, status.as_str(), error, started_iso],
                        )
                    })
                    .map_err(|e| task_error_cause("Could not record schedule task run.", Some(&id), e));
                match written {
                    Ok(n) => {
                        if n > 0 {
                            self.notify_changed();
                            if let Some(tid) = &thread_id {
                                tokio::spawn(self.clone().follow_run(id.clone(), started_iso.clone(), tid.clone(), message_id.clone()));
                            }
                        }
                        find(&store, &id)?.unwrap_or(current)
                    }
                    Err(e) => {
                        self.release_stuck_run(&current, &started_iso, &e.message);
                        return Err(e);
                    }
                }
            }
            Ok(None) => ScheduledTask {
                last_run_at: Some(started_iso.into()),
                last_run_status: status,
                last_run_error: error,
                run_count: active.run_count + 1,
                ..active.clone()
            },
            Err(e) => {
                self.release_stuck_run(&active, &started_iso, &e.message);
                return Err(e);
            }
        };
        Ok(recorded)
    }

    /// `releaseStuckRun`: a run that failed to record still ends.
    fn release_stuck_run(&self, task: &ScheduledTask, started_iso: &str, message: &str) {
        let now = local_now();
        let started = ScheduledTask { last_run_at: Some(started_iso.into()), ..task.clone() };
        let next = next_run_after_run(&started, &now);
        let _ = self.store().db().with(|c| {
            c.execute(
                "UPDATE scheduled_tasks SET last_run_status = 'failed', last_run_error = ?2, next_run_at = ?3, updated_at = ?4,
                   run_count = run_count + 1 WHERE task_id = ?1 AND last_run_status = 'running'",
                params![task.id.as_str(), message, next, iso(now.timestamp())],
            )
        });
        self.notify_changed();
    }

    /// `recordRunFailure`: a dispatch is recorded once the message is accepted;
    /// the provider can still fail the run, which then marks the task failed.
    async fn follow_run(self: Arc<Self>, task_id: String, started_iso: String, thread_id: String, message_id: String) {
        let store = self.store().clone();
        let mut live = store.subscribe();
        let deadline = tokio::time::Instant::now() + RUN_OUTCOME_WAIT;
        loop {
            let Ok(runs) = store.records::<Run>(&thread_id) else { return };
            let Some(run) = runs.into_iter().find(|run| run.user_message_id.as_str() == message_id) else {
                return;
            };
            let generic = match run.status {
                RunStatus::Completed => return,
                RunStatus::Failed => Some("The run failed."),
                RunStatus::Interrupted => Some("The run was stopped."),
                RunStatus::Cancelled => Some("The run was cancelled."),
                RunStatus::RolledBack => Some("The run was rolled back."),
                _ => None,
            };
            if let Some(generic) = generic {
                let items = store.records::<TurnItem>(&thread_id).unwrap_or_default();
                let provider_failure = items.iter().rev().find_map(|item| match &**item {
                    TurnItem::Error(error) if error.run_id.as_ref() == Some(&run.id) => Some(error.failure.message.as_str()),
                    _ => None,
                });
                let message = provider_failure.unwrap_or(generic);
                let n = store.db().with(|c| {
                    c.execute(
                        "UPDATE scheduled_tasks SET last_run_status = 'failed', last_run_error = ?3
                         WHERE task_id = ?1 AND last_run_at = ?2 AND last_run_status = 'succeeded'",
                        params![task_id, started_iso, message],
                    )
                });
                if n.unwrap_or(0) > 0 {
                    self.notify_changed();
                }
                return;
            }
            // Wait for this thread's next event.
            loop {
                match tokio::time::timeout_at(deadline, live.recv()).await {
                    Err(_) => return,
                    Ok(Ok(ev)) if ev.aggregate_kind == "thread" && ev.aggregate_id == thread_id => break,
                    Ok(Ok(_)) => {}
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => break,
                    Ok(Err(broadcast::error::RecvError::Closed)) => return,
                }
            }
        }
    }
}

/// `bindThread`: a continue / fresh-session task keeps the thread its first run made.
fn bind_thread(store: &Store, task: &ScheduledTask, thread_id: &str) {
    let _ = store.db().with(|c| {
        c.execute(
            "UPDATE scheduled_tasks SET thread_id = ?2 WHERE task_id = ?1 AND created_at = ?3 AND project_id = ?4
               AND thread_id IS NULL AND thread_mode != 'new_thread'",
            params![task.id.as_str(), thread_id, task.created_at.as_str(), task.project_id.as_str()],
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::{Value, json};

    /// Central European time with its real DST rules, independent of the host's tzdb.
    fn cet() -> TimeZone {
        TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    fn at(s: &str) -> Zoned {
        s.parse::<Timestamp>().unwrap().to_zoned(cet())
    }

    fn schedule(sent: Value) -> ScheduledTaskSchedule {
        serde_json::from_value(sent).expect("a schedule")
    }

    fn next(sent: Value, from: &str) -> Option<String> {
        next_scheduled_run(&schedule(sent), &at(from)).map(iso)
    }

    #[test]
    fn interval_adds_elapsed_time_with_a_one_minute_floor() {
        let from = "2026-03-29T00:30:00Z";
        assert_eq!(next(json!({ "type": "interval", "everyMs": 3_600_000 }), from).as_deref(), Some("2026-03-29T01:30:00.000Z"));
        assert_eq!(next(json!({ "type": "interval", "everyMs": 5 }), from).as_deref(), Some("2026-03-29T00:31:00.000Z"));
        assert_eq!(next(json!({ "type": "interval", "everyMs": i64::MAX }), from), None, "unrepresentable");
        assert_eq!(next(json!({ "type": "interval", "everyMs": u64::MAX }), from), None, "unrepresentable");
    }

    #[test]
    fn fixed_time_is_local_wall_clock_across_dst() {
        // 09:00 CET is 08:00Z in winter and 07:00Z after the March switch.
        let s = json!({ "type": "fixed_time", "timeOfDay": "9:00" });
        assert_eq!(next(s.clone(), "2026-03-28T07:30:00Z").as_deref(), Some("2026-03-28T08:00:00.000Z"));
        assert_eq!(next(s.clone(), "2026-03-28T08:00:00Z").as_deref(), Some("2026-03-29T07:00:00.000Z"), "strictly after");
        // 02:30 does not exist on the switch day; it resolves forward (compatible disambiguation).
        let gap = json!({ "type": "fixed_time", "timeOfDay": "02:30" });
        assert_eq!(next(gap, "2026-03-28T12:00:00Z").as_deref(), Some("2026-03-29T01:30:00.000Z"));
    }

    #[test]
    fn fixed_time_honours_weekdays() {
        // 2026-11-13 is a Friday; weekdays use 0 = Sunday.
        let s = json!({ "type": "fixed_time", "timeOfDay": "10:15", "weekdays": [1, 3] });
        assert_eq!(next(s, "2026-11-13T12:00:00Z").as_deref(), Some("2026-11-16T09:15:00.000Z"));
        let every_day = json!({ "type": "fixed_time", "timeOfDay": "23:59", "weekdays": [] });
        assert_eq!(next(every_day, "2026-11-13T12:00:00Z").as_deref(), Some("2026-11-13T22:59:00.000Z"));
        assert_eq!(next(json!({ "type": "fixed_time", "timeOfDay": "24:00" }), "2026-11-13T12:00:00Z"), None);
        assert_eq!(next(json!({ "type": "fixed_time", "timeOfDay": "9:5" }), "2026-11-13T12:00:00Z"), None);
    }

    #[test]
    fn once_and_webhook() {
        let s = json!({ "type": "once", "runAt": "2026-11-14T10:00:00+01:00" });
        assert_eq!(next(s.clone(), "2026-11-13T12:00:00Z").as_deref(), Some("2026-11-14T09:00:00.000Z"));
        assert_eq!(next(s, "2026-11-15T00:00:00Z"), None);
        assert_eq!(next(json!({ "type": "once", "runAt": "soon" }), "2026-11-15T00:00:00Z"), None);
        assert_eq!(next(json!({ "type": "webhook", "signature": null }), "2026-11-13T12:00:00Z"), None);
        assert_eq!(next(json!({ "type": "cron", "expression": "* * * * *" }), "2026-11-13T12:00:00Z"), None, "a kind this build does not know");
    }

    #[test]
    fn same_schedule_compares_meaning() {
        let same = |a: Value, b: Value| is_same_schedule(&schedule(a), &schedule(b));
        let a = json!({ "type": "fixed_time", "timeOfDay": "09:00", "weekdays": [1, 2, 3, 4, 5, 6, 0] });
        let b = json!({ "type": "fixed_time", "timeOfDay": "9:00" });
        assert!(same(a, b.clone()), "padded hour, all weekdays = daily");
        assert!(!same(b, json!({ "type": "fixed_time", "timeOfDay": "9:00", "weekdays": [1] })));
        assert!(same(json!({ "type": "once", "runAt": "2026-11-14T10:00:00+01:00" }), json!({ "type": "once", "runAt": "2026-11-14T09:00:00.000Z" })));
        assert!(!same(json!({ "type": "once", "runAt": "soon" }), json!({ "type": "once", "runAt": "soon" })), "no time, nothing to keep");
        assert!(!same(json!({ "type": "interval", "everyMs": 60000 }), json!({ "type": "interval", "everyMs": 60001 })));
        assert!(same(json!({ "type": "webhook", "signature": null }), json!({ "type": "webhook", "signature": null, "filters": [{ "op": "exists", "path": "body" }] })));
        assert!(!same(json!({ "type": "interval", "everyMs": 60000 }), json!({ "type": "webhook", "signature": null })));
        assert!(!same(json!({ "type": "cron" }), json!({ "type": "cron" })));
    }

    #[test]
    fn missed_fixed_time_has_a_ten_minute_grace() {
        let due: Timestamp = "2026-11-13T09:00:00Z".parse().unwrap();
        let s = schedule(json!({ "type": "fixed_time", "timeOfDay": "10:00" }));
        assert!(!is_missed_fixed_time_run(&s, due, "2026-11-13T09:10:00Z".parse().unwrap()));
        assert!(is_missed_fixed_time_run(&s, due, "2026-11-13T09:10:01Z".parse().unwrap()));
        assert!(!is_missed_fixed_time_run(&schedule(json!({ "type": "interval", "everyMs": 60000 })), due, "2026-11-14T00:00:00Z".parse().unwrap()));
    }

    #[test]
    fn failures_are_written_as_the_tagged_error_of_t3code() {
        assert_eq!(json!(task_error("Could not list.", None)), json!({ "_tag": "ScheduledTaskError", "message": "Could not list." }));
        assert_eq!(
            json!(task_error("Schedule task not found.", Some("task-1"))),
            json!({ "_tag": "ScheduledTaskError", "message": "Schedule task not found.", "taskId": "task-1" })
        );
        let refused = task_error_cause("Could not save schedule task.", Some("task-1"), "NOT NULL constraint failed: scheduled_tasks.title");
        assert_eq!(
            json!(refused),
            json!({ "_tag": "ScheduledTaskError", "message": "Could not save schedule task.", "taskId": "task-1", "cause": "NOT NULL constraint failed: scheduled_tasks.title" })
        );
        assert_eq!(json!(task_error_cause("Could not list schedule tasks.", None, "no such table")), json!({ "_tag": "ScheduledTaskError", "message": "Could not list schedule tasks.", "cause": "no such table" }));
        assert_eq!(refused.to_string(), "Could not save schedule task.");
        // The log gets the failure as the client does, keys in this order.
        assert_eq!(
            refused.written(),
            r#"{"_tag":"ScheduledTaskError","message":"Could not save schedule task.","taskId":"task-1","cause":"NOT NULL constraint failed: scheduled_tasks.title"}"#
        );
    }

    fn input(schedule: Value) -> Value {
        json!({
            "id": "task-1", "title": "Daily", "prompt": "Summarize", "enabled": true, "schedule": schedule,
            "projectId": "p1", "workspaceStrategy": { "type": "root" },
            "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
            "runtimeMode": "full-access", "interactionMode": "default",
        })
    }

    /// An upsert of a payload as a client sends it; the saved task as the client gets it.
    fn save_sent(store: &Store, sent: &Value, now: &Zoned) -> Result<Value, ScheduledTaskError> {
        upsert(store, serde_json::from_value(sent.clone()).expect("a payload that decodes"), now).map(|task| json!(task))
    }

    #[test]
    fn upsert_keeps_the_pending_run_when_the_schedule_is_unchanged() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let t0 = at("2026-11-13T12:00:00Z");
        let task = save_sent(&store, &input(json!({ "type": "interval", "everyMs": 3_600_000 })), &t0).unwrap();
        assert_eq!(task["nextRunAt"], "2026-11-13T13:00:00.000Z");
        assert_eq!((task["threadMode"].as_str(), task["lastRunStatus"].as_str(), task["createdBy"].as_str()), (Some("new_thread"), Some("never"), Some("user")));
        // A title edit later does not postpone the run; a schedule edit does.
        let mut edit = input(json!({ "type": "interval", "everyMs": 3_600_000 }));
        edit["title"] = json!("Renamed");
        let t1 = at("2026-11-13T12:30:00Z");
        let task = save_sent(&store, &edit, &t1).unwrap();
        assert_eq!((task["title"].as_str(), task["nextRunAt"].as_str()), (Some("Renamed"), Some("2026-11-13T13:00:00.000Z")));
        edit["schedule"] = json!({ "type": "interval", "everyMs": 7_200_000 });
        let task = save_sent(&store, &edit, &t1).unwrap();
        assert_eq!(task["nextRunAt"], "2026-11-13T14:30:00.000Z");
        assert_eq!(task["createdAt"], "2026-11-13T12:00:00.000Z");
        // Disabling clears the next run; enabling restarts the clock.
        let (task, changed) = set_enabled(&store, "task-1", false, &t1).unwrap();
        assert!(changed && task.next_run_at.is_none());
        assert!(!list(&store).unwrap()[0].enabled);
        delete(&store, "task-1").unwrap();
        assert!(find(&store, "task-1").unwrap().is_none());
    }

    #[test]
    fn upsert_validates_once_and_webhook_tasks() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let now = at("2026-11-13T12:00:00Z");
        let past = save_sent(&store, &input(json!({ "type": "once", "runAt": "2026-11-13T11:00:00Z" })), &now).unwrap_err();
        assert_eq!(past.message, "The run time has passed. Pick a time in the future.");
        let mut missing = input(json!({ "type": "webhook" }));
        missing["requireExisting"] = json!(true);
        assert_eq!(save_sent(&store, &missing, &now).unwrap_err(), task_error("Schedule task not found.", Some("task-1")));

        let hook = input(json!({ "type": "webhook", "signature": { "header": "X-Hub-Signature-256", "encoding": "hex", "prefix": "sha256=", "secret": "s" } }));
        let task = save_sent(&store, &hook, &now).unwrap();
        assert_eq!(task["schedule"]["signature"]["header"], "x-hub-signature-256");
        assert!(task["schedule"]["signature"].get("secret").is_none(), "secrets never leave the server");
        assert_eq!(task["webhook"]["hasSecret"], true);
        assert!(task["nextRunAt"].is_null());
        let path = task["webhook"]["path"].as_str().unwrap().to_owned();
        assert!(path.starts_with("/api/hooks/task-1/"), "{path}");
        let rotated = json!(rotate_webhook_token(&store, "task-1", &now).unwrap());
        assert_ne!(rotated["webhook"]["path"], json!(path));
        // A re-save keeps the stored secret and token.
        let mut resave = hook.clone();
        resave["schedule"]["signature"].as_object_mut().unwrap().remove("secret");
        let task = save_sent(&store, &resave, &now).unwrap();
        assert_eq!((task["webhook"]["hasSecret"].clone(), task["webhook"]["path"].clone()), (json!(true), rotated["webhook"]["path"].clone()));
        let mut interval = input(json!({ "type": "interval", "everyMs": 60000 }));
        interval["id"] = json!("task-2");
        save_sent(&store, &interval, &now).unwrap();
        assert_eq!(rotate_webhook_token(&store, "task-2", &now).unwrap_err().message, "Only webhook tasks have a URL token.");
    }

    #[test]
    fn a_webhook_upsert_keeps_what_it_does_not_send() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let now = at("2026-11-13T12:00:00Z");
        let dedupe = json!({ "replayKey": "{{headers.x-delivery}}" });
        let filters = json!([{ "path": "body.ref", "op": "equals", "value": "main" }, { "path": "body.kind", "op": "in", "value": ["a"] }, { "path": "query.t", "op": "exists" }]);
        let hook = |more: Value| {
            let mut sent = json!({ "type": "webhook" });
            sent.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
            input(sent)
        };
        let task = save_sent(&store, &hook(json!({ "maxDeliveryAgeMinutes": 60, "dedupe": dedupe, "filters": filters })), &now).unwrap();
        assert_eq!(task["schedule"], json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": 60, "dedupe": dedupe, "filters": filters }));
        // Nothing sent: the age limit is not kept, the templates and filters are.
        let task = save_sent(&store, &hook(json!({})), &now).unwrap();
        assert_eq!(task["schedule"], json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null, "dedupe": dedupe, "filters": filters }));
        // `null` filters are filters not sent; `null` templates are no templates.
        let task = save_sent(&store, &hook(json!({ "filters": null, "dedupe": null })), &now).unwrap();
        assert_eq!(task["schedule"], json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null, "filters": filters }));
        let task = save_sent(&store, &hook(json!({ "filters": [] })), &now).unwrap();
        assert_eq!(task["schedule"], json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null }));
        // A secret request is something Domovoi cannot answer; a signature needs a secret.
        let signature = json!({ "header": "x-sig", "encoding": "base64", "prefix": "" });
        let mut with_ref = signature.clone();
        with_ref["secretRef"] = json!("secret-1");
        assert_eq!(save_sent(&store, &hook(json!({ "signature": with_ref })), &now).unwrap_err().message, "That secret request is no longer available. Ask for the secret again.");
        assert_eq!(save_sent(&store, &hook(json!({ "signature": signature })), &now).unwrap_err().message, "A webhook signature check needs a signing secret.");
    }

    #[test]
    fn an_upsert_reads_its_payload_as_leniently_as_before() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let now = at("2026-11-13T12:00:00Z");
        // Fields of another type count as not sent: the id comes from the
        // command id, the task is disabled, made by the user on the web.
        let mut sent = input(json!({ "type": "interval", "everyMs": 60_000 }));
        for (key, value) in [("id", json!(7)), ("commandId", json!("c1")), ("enabled", json!("yes")), ("createdBy", json!(null)), ("creationSource", json!(1)), ("threadMode", json!(false)), ("threadId", json!(null))] {
            sent[key] = value;
        }
        sent.as_object_mut().unwrap().remove("workspaceStrategy");
        let task = save_sent(&store, &sent, &now).unwrap();
        assert_eq!((task["id"].as_str(), task["enabled"].as_bool(), task["nextRunAt"].is_null()), (Some("scheduled-task:c1"), Some(false), true));
        assert_eq!((task["createdBy"].as_str(), task["creationSource"].as_str(), task["threadMode"].as_str()), (Some("user"), Some("web"), Some("new_thread")));
        assert_eq!(task["workspaceStrategy"], json!({ "type": "root" }), "a task saved without one works in the project's folder");
        let stored: String = store.db().with(|c| c.query_row("SELECT workspace_strategy_json FROM scheduled_tasks", [], |r| r.get(0))).unwrap();
        assert_eq!(stored, "null", "and its row says so, as it did");
        // What the table has no default for, it refuses.
        for (key, column) in [("title", "title"), ("prompt", "prompt"), ("runtimeMode", "runtime_mode"), ("interactionMode", "interaction_mode")] {
            let mut sent = input(json!({ "type": "interval", "everyMs": 60_000 }));
            sent.as_object_mut().unwrap().remove(key);
            let refused = save_sent(&store, &sent, &now).unwrap_err();
            assert_eq!(refused, task_error_cause("Could not save schedule task.", Some("task-1"), format!("NOT NULL constraint failed: scheduled_tasks.{column}")));
        }
        // A schedule or a model selection that is not one is not a payload.
        for (key, value) in [("schedule", json!({ "type": "interval" })), ("schedule", json!("hourly")), ("schedule", json!({ "type": "webhook", "signature": "none" })), ("modelSelection", json!(null))] {
            let mut sent = input(json!({ "type": "interval", "everyMs": 60_000 }));
            sent[key] = value;
            assert!(serde_json::from_value::<UpsertInput>(sent.clone()).is_err(), "{sent}");
        }
        // A kind of schedule this build does not know is kept as sent and never due.
        let mut sent = input(json!({ "type": "cron", "expression": "0 9 * * *" }));
        sent["id"] = json!("task-cron");
        let task = save_sent(&store, &sent, &now).unwrap();
        assert_eq!((task["schedule"].clone(), task["nextRunAt"].clone()), (json!({ "type": "cron", "expression": "0 9 * * *" }), json!(null)));
    }

    /// A row as the server wrote it before its tasks were records: the JSON
    /// columns hold what the client sent, in the client's key order.
    fn insert_row(store: &Store, id: &str, schedule: &str, strategy: &str, model: &str, token: Option<&str>) {
        store
            .db()
            .with(|c| {
                c.execute(
                    "INSERT INTO scheduled_tasks VALUES (?1, 'Old', 'Summarize', 1, ?2, 'p1', NULL, 'new_thread', ?3, ?4, 'full-access', 'plan', 'agent', 'mcp',
                       '2025-03-01T08:00:00.000Z', '2025-03-02T09:30:00.500Z', '2999-01-01T00:00:00.000Z', NULL, 'never', NULL, 3, ?5, NULL)",
                    params![id, schedule, strategy, model, token],
                )
            })
            .unwrap();
    }

    #[test]
    fn rows_of_the_server_before_tasks_were_records_are_read_as_they_were_sent() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let model = r#"{"model":"gpt-5","instanceId":"codex","options":[{"id":"reasoningEffort","value":"high"},{"id":"fastMode","value":true}]}"#;
        let rows = [
            ("a", r#"{"everyMs":7200000,"type":"interval"}"#, r#"{"type":"worktree","baseRef":"main","branch":"nightly","startFromOrigin":true}"#),
            ("b", r#"{"type":"fixed_time","timeOfDay":"09:30","weekdays":[1,2,3,4,5]}"#, r#"{"type":"existing_worktree","worktreePath":"/w","branch":"main"}"#),
            ("c", r#"{"type":"fixed_time","timeOfDay":"7:05"}"#, r#"{"type":"root"}"#),
            ("d", r#"{"type":"once","runAt":"2999-02-03T04:05:06.000Z"}"#, r#"{"type":"root","branch":"main"}"#),
            ("e", r#"{"type":"webhook","signature":null,"maxDeliveryAgeMinutes":null}"#, r#"{"type":"root"}"#),
            (
                "f",
                r#"{"type":"webhook","signature":{"header":"x-sig","encoding":"hex","prefix":"sha256="},"maxDeliveryAgeMinutes":60,"dedupe":{"replayKey":"{{request}}","coalesceKey":"{{body.ref}}","coalesceWindowSeconds":30},"filters":[{"path":"body.ref","op":"equals","value":"main"},{"path":"body.a","op":"notEquals","value":"x"},{"path":"headers.e","op":"startsWith","value":"p"},{"path":"body.kind","op":"in","value":["a","b c"]},{"path":"query.t","op":"exists"}]}"#,
                r#"{"type":"root"}"#,
            ),
            ("g", r#"{"type":"cron","expression":"0 9 * * *"}"#, r#"{"type":"root"}"#),
        ];
        for (id, schedule, strategy) in rows {
            insert_row(&store, id, schedule, strategy, model, Some("tok"));
        }
        let listed = list(&store).unwrap();
        assert_eq!(listed.len(), rows.len());
        for ((id, schedule, strategy), task) in rows.iter().zip(&listed) {
            let webhook = schedule.contains("\"webhook\"");
            let mut expected = json!({
                "id": id, "title": "Old", "prompt": "Summarize", "enabled": true, "schedule": serde_json::from_str::<Value>(schedule).unwrap(),
                "projectId": "p1", "threadId": null, "threadMode": "new_thread", "workspaceStrategy": serde_json::from_str::<Value>(strategy).unwrap(),
                "modelSelection": serde_json::from_str::<Value>(model).unwrap(), "runtimeMode": "full-access", "interactionMode": "plan",
                "createdBy": "agent", "creationSource": "mcp", "createdAt": "2025-03-01T08:00:00.000Z", "updatedAt": "2025-03-02T09:30:00.500Z",
                "nextRunAt": "2999-01-01T00:00:00.000Z", "lastRunAt": null, "lastRunStatus": "never", "lastRunError": null, "runCount": 3,
            });
            if webhook {
                expected["webhook"] = json!({ "path": format!("/api/hooks/{id}/tok"), "url": null, "hasSecret": false });
            }
            assert_eq!(json!(task), expected, "{id}");
        }
        // An edit that keeps the schedule names the row's own spelling of it,
        // so the pending run stays where the row has it.
        let mut edit = input(json!({ "type": "interval", "everyMs": 7_200_000 }));
        edit["id"] = json!("a");
        let saved = save_sent(&store, &edit, &at("2026-11-13T12:00:00Z")).unwrap();
        assert_eq!((saved["nextRunAt"].as_str(), saved["createdBy"].as_str(), saved["runCount"].as_u64()), (Some("2999-01-01T00:00:00.000Z"), Some("agent"), Some(3)));
        let text: String = store.db().with(|c| c.query_row("SELECT schedule_json FROM scheduled_tasks WHERE task_id = 'a'", [], |r| r.get(0))).unwrap();
        assert_eq!(text, r#"{"type":"interval","everyMs":7200000}"#);
    }

    #[test]
    fn a_row_that_is_no_task_is_left_out_of_lists_and_cannot_be_loaded() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let model = r#"{"instanceId":"codex","model":"gpt-5"}"#;
        insert_row(&store, "good", r#"{"type":"interval","everyMs":60000}"#, "null", model, None);
        insert_row(&store, "no-schedule", "null", r#"{"type":"root"}"#, model, None);
        insert_row(&store, "half-a-schedule", r#"{"type":"interval"}"#, r#"{"type":"root"}"#, model, None);
        insert_row(&store, "no-model", r#"{"type":"interval","everyMs":60000}"#, r#"{"type":"root"}"#, "null", None);
        store.db().with(|c| c.execute("UPDATE scheduled_tasks SET next_run_at = '2020-01-01T00:00:00.000Z'", [])).unwrap();
        let listed = list(&store).unwrap();
        assert_eq!(listed.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["good"]);
        assert_eq!(listed[0].workspace_strategy, WorkspaceStrategy::Root { branch: None }, "saved without one: the project's folder");
        let now = at("2026-11-13T12:00:00Z");
        assert_eq!(due_tasks(&store, &now).unwrap().len(), 1, "the timer goes on with the others");
        let still_due: i64 = store.db().with(|c| c.query_row("SELECT COUNT(*) FROM scheduled_tasks WHERE next_run_at IS NOT NULL", [], |r| r.get(0))).unwrap();
        assert_eq!(still_due, 1, "and does not look at the unreadable ones again");
        let unreadable = find(&store, "half-a-schedule").unwrap_err();
        assert_eq!((unreadable.message.as_str(), unreadable.task_id.as_deref(), unreadable.cause.is_some()), ("Could not load schedule task.", Some("half-a-schedule"), true));
        // It can still be deleted, or saved over.
        delete(&store, "no-model").unwrap();
        assert!(find(&store, "no-model").unwrap().is_none());
    }

    #[test]
    fn a_reservation_holds_the_task_until_dropped_and_fails_an_unrecorded_run() {
        let store = Store::new(Db::open_in_memory().unwrap());
        save_sent(&store, &input(json!({ "type": "interval", "everyMs": 60_000 })), &at("2026-11-13T12:00:00Z")).unwrap();
        let task = find(&store, "task-1").unwrap().unwrap();
        let active = Arc::default();
        let held = Reservation::take(&active, &store, &task).expect("free");
        assert!(Reservation::take(&active, &store, &task).is_none(), "a running task does not start again");
        store.db().with(|c| c.execute("UPDATE scheduled_tasks SET last_run_status = 'running'", [])).unwrap();
        drop(held); // the run ended without recording (a panic)
        assert_eq!(find(&store, "task-1").unwrap().unwrap().last_run_status, ScheduledTaskRunStatus::Failed);
        let mut again = Reservation::take(&active, &store, &task).expect("free again");
        again.recorded = true;
        drop(again);
        assert!(crate::util::locked(&active).is_empty());
    }

    #[test]
    fn restart_recovery_fails_running_rows_and_moves_them_on() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let now = at("2026-11-13T12:00:00Z");
        save_sent(&store, &input(json!({ "type": "interval", "everyMs": 60_000 })), &now).unwrap();
        store.db().with(|c| c.execute("UPDATE scheduled_tasks SET last_run_status = 'running', last_run_at = '2026-11-13T12:01:00.000Z'", [])).unwrap();
        let later = at("2026-11-13T13:00:00Z");
        assert!(due_tasks(&store, &later).unwrap().is_empty(), "running rows are never due");
        assert_eq!(recover_interrupted(&store, &later), 1);
        let task = find(&store, "task-1").unwrap().unwrap();
        assert_eq!((task.last_run_status.as_str(), task.run_count), ("failed", 1));
        assert_eq!(task.last_run_error.as_deref(), Some("Run was interrupted by a server restart."));
        assert_eq!(task.next_run_at.as_ref().map(WireTime::as_str), Some("2026-11-13T13:01:00.000Z"));
        assert_eq!(due_tasks(&store, &at("2026-11-13T13:01:00Z")).unwrap().len(), 1);
    }
}
