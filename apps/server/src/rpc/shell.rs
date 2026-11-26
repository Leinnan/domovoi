//! `orchestration.subscribeShell` and project mutations.
//!
//! Stream rules follow t3code's `subscribeOrchestrationV2Shell`:
//! - no `afterSequence`: snapshot item(s), completion marker, live tail;
//! - resumable `afterSequence`: identity-roots snapshot item, replayed items,
//!   completion marker, live tail;
//! - otherwise `snapshot-required` (when `httpSnapshotFallback`) or a snapshot.
//! Live events are subscribed before reading so nothing between the read and
//! the subscription is lost; duplicates are dropped by sequence.

#![expect(clippy::disallowed_types, reason = "a client's command or launch as JSON, where it is decoded into `Command`: one that does not fit is a typed failure")]

use crate::orchestration::checkpoints::DiffCall;
use crate::orchestration::commands::{DispatchFailure, DispatchResult, LaunchFailure, LaunchRefusal, LaunchResult, Orchestrator, Refusal};
use crate::orchestration::failure::Failure;
use crate::state::{AppState, ConnCtx};
use crate::store::StoreError;
use crate::vcs;
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::sync::Arc;
use crate::wire::shell::{ProjectRecord, ProjectShell, SHELL_SCHEMA_VERSION, ShellSnapshot, ShellStreamItem};
use domovoi_model::typed::{Command, ModelSelection, ProjectIcon, ProjectScript, ThreadEnvMode, ThreadLaunchInput, ThreadShell};
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShellSubscription {
    after_sequence: Option<u64>,
    #[serde(default)]
    http_snapshot_fallback: bool,
    #[serde(default)]
    request_completion_marker: bool,
}

pub fn snapshot(state: &AppState) -> Result<ShellSnapshot, StoreError> {
    snapshot_of(&state.store)
}

/// `snapshot` on the blocking pool: it reads every thread, which with many
/// threads takes long enough to hold up the connections sharing a worker.
pub async fn snapshot_off_runtime(state: crate::state::SharedState) -> Result<ShellSnapshot, StoreError> {
    tokio::task::spawn_blocking(move || snapshot(&state))
        .await
        .unwrap_or_else(|e| Err(StoreError::Invalid(format!("the shell snapshot did not finish: {e}"))))
}

/// The shell snapshot of everything in `store`.
pub fn snapshot_of(store: &crate::store::Store) -> Result<ShellSnapshot, StoreError> {
    // Read the sequence first: anything appended after it is replayed by the tail.
    let snapshot_sequence = store.latest_sequence()?;
    let mut threads = vec![];
    let mut archived_threads = vec![];
    for (id, last_sequence) in store.thread_ids()? {
        if let Some(shell) = store.snapshot_shell(&id, last_sequence)? {
            if shell.archived_at.is_none() { threads.push(shell) } else { archived_threads.push(shell) }
        }
    }
    Ok(ShellSnapshot {
        schema_version: SHELL_SCHEMA_VERSION,
        snapshot_sequence,
        threads,
        archived_threads,
        projects: store.list_projects()?,
    })
}

/// The roots whose repository identity is resolved: every project's, once each.
fn identity_roots(projects: &[ProjectShell]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::with_capacity(projects.len());
    for p in projects {
        if !roots.contains(&p.workspace_root) {
            roots.push(p.workspace_root.clone());
        }
    }
    roots
}

/// The shell with its projects only: what a resumed stream's identity item needs.
/// No thread is read.
fn projects_snapshot(store: &crate::store::Store) -> Result<ShellSnapshot, StoreError> {
    let snapshot_sequence = store.latest_sequence()?;
    Ok(ShellSnapshot { schema_version: SHELL_SCHEMA_VERSION, snapshot_sequence, threads: vec![], archived_threads: vec![], projects: store.list_projects()? })
}

async fn projects_snapshot_off_runtime(state: crate::state::SharedState) -> Result<ShellSnapshot, StoreError> {
    tokio::task::spawn_blocking(move || projects_snapshot(&state.store))
        .await
        .unwrap_or_else(|e| Err(StoreError::Invalid(format!("the shell projects did not finish: {e}"))))
}

/// The repository-identity item (`shellStreamItemsFromInitialSnapshot`'s
/// second item): the projects whose roots resolved and those roots, with no
/// thread. It only updates project metadata, so it repeats no thread row.
fn identity_item(snap: ShellSnapshot) -> Option<ShellStreamItem> {
    let roots = identity_roots(&snap.projects);
    if roots.is_empty() {
        return None;
    }
    let projects = snap.projects.into_iter().filter(|p| roots.contains(&p.workspace_root)).collect();
    Some(ShellStreamItem::Snapshot {
        snapshot: Box::new(ShellSnapshot { threads: vec![], archived_threads: vec![], projects, ..snap }),
        resolved_repository_identity_roots: Some(roots),
    })
}

/// `shellStreamItemsFromInitialSnapshot`: the authoritative snapshot of the
/// active threads, then the repository-identity item. Archived threads are not
/// on the stream (as in t3code): `getArchivedShellSnapshot` serves them.
pub(crate) fn initial_items(snap: ShellSnapshot) -> Vec<ShellStreamItem> {
    let snap = ShellSnapshot { archived_threads: vec![], ..snap };
    let identity = identity_item(ShellSnapshot {
        schema_version: snap.schema_version,
        snapshot_sequence: snap.snapshot_sequence,
        threads: vec![],
        archived_threads: vec![],
        projects: snap.projects.clone(),
    });
    let mut items = vec![ShellStreamItem::Snapshot { snapshot: Box::new(snap), resolved_repository_identity_roots: None }];
    items.extend(identity);
    items
}

/// What the shell stream sends for the event `sequence`, which changed the
/// `aggregate_kind` record `aggregate_id`: that record as it is now.
fn item_for_event(state: &AppState, sequence: u64, aggregate_kind: &str, aggregate_id: &str, event_type: &str) -> Option<ShellStreamItem> {
    match aggregate_kind {
        "thread" => {
            let shell = state.store.thread_shell(aggregate_id).ok().flatten();
            Some(match shell {
                Some(thread) if thread.deleted_at.is_none() => {
                    let location = if thread.archived_at.is_none() { "active" } else { "archive" };
                    ShellStreamItem::ThreadUpdated { sequence, location: location.into(), thread }
                }
                _ => ShellStreamItem::ThreadRemoved { sequence, location: "active".into(), thread_id: aggregate_id.to_owned() },
            })
        }
        "project" => Some(match state.store.get_project(aggregate_id).ok().flatten() {
            Some(project) if event_type != "project.deleted" => ShellStreamItem::ProjectUpdated { sequence, project: Box::new(project) },
            _ => ShellStreamItem::ProjectRemoved { sequence, project_id: aggregate_id.to_owned() },
        }),
        _ => None,
    }
}

/// Whether two shells of a thread say the same apart from `updatedAt`.
/// Every field is named: one added to the shell does not compile until it is
/// compared here (or listed as left out).
fn same_apart_from_updated_at(a: &ThreadShell, b: &ThreadShell) -> bool {
    macro_rules! same {
        ($($field:ident),+ ; ignoring $($ignored:ident),+) => {{
            let ThreadShell { $($field: _,)+ $($ignored: _,)+ } = a;
            $(a.$field == b.$field)&&+
        }};
    }
    same!(
        created_by, creation_source, id, project_id, title, provider_instance_id, model_selection, runtime_mode, interaction_mode,
        branch, worktree_path, linked_pull_request, pull_requests, issues, branch_pull_request, lineage, forked_from,
        active_provider_thread_id, history_origin, link_origin, delegated_from, latest_run_id, latest_run_requested_at,
        latest_run_started_at, latest_run_completed_at, latest_run_stop_requested, latest_run_background_wake, active_run_id,
        activity_run_started_at, activity_run_status, blocked_on_thread_wait, status, last_error, last_error_class, last_error_code,
        usage_limit_reset_at, pending_runtime_request, pending_guest_message_count, latest_visible_message, latest_user_message_at,
        latest_user_authored_message_at, has_actionable_proposed_plan, pending_background_tasks, provider_instance_history, goal,
        item_count, visible_item_count, created_at, archived_at, settled_override, settled_at, unsettled_at, snoozed_until,
        snoozed_at, limit_recovery, pinned_at, auto_settle_disabled_at, pin_order_key, active_order_key, tags, last_visited_at,
        opens, title_regeneration, last_title_regeneration, deleted_at;
        ignoring updated_at
    )
}

fn shell_stream(ctx: Arc<ConnCtx>, input: ShellSubscription) -> BoxStream<'static, Result<ShellStreamItem, Failure>> {
    let state = ctx.state.clone();
    async_stream::stream! {
        let mut live = state.store.subscribe();
        let high_water = match state.store.latest_sequence() { Ok(s) => s, Err(e) => { yield Err(failure(e)); return; } };
        let mut last_sent;
        match input.after_sequence {
            Some(after) if after <= high_water && state.store.replays_from(after).unwrap_or(false) => {
                // The log still reaches back to where the client was.
                let snap = match projects_snapshot_off_runtime(state.clone()).await { Ok(s) => s, Err(e) => { yield Err(failure(e)); return; } };
                if let Some(item) = identity_item(snap) { yield Ok(item); }
                match state.store.events_after(after, high_water) {
                    Ok(events) => for e in &events {
                        if let Some(item) = item_for_event(&state, e.sequence, &e.aggregate_kind, &e.aggregate_id, &e.event_type) { yield Ok(item); }
                    },
                    Err(e) => { yield Err(failure(e)); return; }
                }
                last_sent = high_water;
            }
            Some(_) if input.http_snapshot_fallback => {
                yield Ok(ShellStreamItem::SnapshotRequired);
                return;
            }
            _ => {
                let snap = match snapshot_off_runtime(state.clone()).await { Ok(s) => s, Err(e) => { yield Err(failure(e)); return; } };
                last_sent = snap.snapshot_sequence;
                for item in initial_items(snap) { yield Ok(item); }
            }
        }
        if input.request_completion_marker {
            yield Ok(ShellStreamItem::Synchronized);
        }
        // The shell last sent for each thread, and when.
        let mut sent: std::collections::HashMap<String, (Arc<ThreadShell>, std::time::Instant)> = Default::default();
        loop {
            match live.recv().await {
                Ok(e) if e.sequence <= last_sent => {}
                Ok(e) => {
                    last_sent = e.sequence;
                    if let Some(item) = item_for_event(&state, e.sequence, &e.aggregate_kind, &e.aggregate_id, &e.event_type) {
                        if let ShellStreamItem::ThreadUpdated { thread, .. } = &item {
                            // skipUnchangedThreadShells: drop identical shells (ignoring updatedAt) for 5s.
                            if let Some((previous, at)) = sent.get(&e.aggregate_id) {
                                if at.elapsed() < std::time::Duration::from_secs(5) && same_apart_from_updated_at(previous, thread) { continue; }
                            }
                            sent.insert(e.aggregate_id.clone(), (thread.clone(), std::time::Instant::now()));
                        }
                        yield Ok(item);
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Too slow to keep up: hand the client a fresh snapshot.
                    if let Ok(snap) = snapshot_off_runtime(state.clone()).await {
                        last_sent = snap.snapshot_sequence;
                        for item in initial_items(snap) { yield Ok(item); }
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

fn failure(e: impl std::fmt::Display) -> Failure {
    Failure::ShellSnapshot { message: format!("Failed to prepare the application shell stream: {e}") }
}

/// What a diff between two turns is asked with. A field left out, or not of
/// its type, reads as empty or zero, and the diff then fails by its own rules.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnDiffInput {
    #[serde(default, deserialize_with = "text_or_empty")]
    thread_id: String,
    #[serde(default, deserialize_with = "count_or_zero")]
    from_turn_count: u64,
    #[serde(default, deserialize_with = "count_or_zero")]
    to_turn_count: u64,
}

/// What a field that may hold anything was sent as: a `T`, or something else.
#[derive(Deserialize)]
#[serde(untagged)]
enum Loose<T> {
    Fits(T),
    Other(IgnoredAny),
}

impl<T> Loose<T> {
    fn fits(self) -> Option<T> {
        match self {
            Loose::Fits(value) => Some(value),
            Loose::Other(_) => None,
        }
    }
}

/// Reads a field that may hold anything: its value when that is a `T`, and
/// `None` for anything else (with `#[serde(default)]`, for a field left out too).
pub(crate) fn if_fits<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Option<T>, D::Error> {
    Ok(Loose::<T>::deserialize(sent)?.fits())
}

/// Reads a list of which the elements that are no `T` are left out; anything
/// but a list reads as an empty one.
pub(crate) fn those_that_fit<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Vec<T>, D::Error> {
    Ok(list_if_list::<Loose<T>, D>(sent)?.unwrap_or_default().into_iter().filter_map(Loose::fits).collect())
}

/// The `visit_*` of a visitor for what it does not read: each gives `$otherwise`.
/// A list or a record is read to its end and passed over.
macro_rules! otherwise {
    ($otherwise:expr; $($visit:ident),+) => {
        $(otherwise!(@ $otherwise; $visit);)+
    };
    (@ $otherwise:expr; bool) => {
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok($otherwise)
        }
    };
    (@ $otherwise:expr; number) => {
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok($otherwise)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok($otherwise)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok($otherwise)
        }
    };
    (@ $otherwise:expr; text) => {
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok($otherwise)
        }
    };
    (@ $otherwise:expr; null) => {
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok($otherwise)
        }
    };
    (@ $otherwise:expr; list) => {
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, list: A) -> Result<Self::Value, A::Error> {
            IgnoredAny::deserialize(serde::de::value::SeqAccessDeserializer::new(list)).map(|_| $otherwise)
        }
    };
    (@ $otherwise:expr; record) => {
        fn visit_map<A: serde::de::MapAccess<'de>>(self, record: A) -> Result<Self::Value, A::Error> {
            IgnoredAny::deserialize(serde::de::value::MapAccessDeserializer::new(record)).map(|_| $otherwise)
        }
    };
}

/// Reads a field that may hold anything as its text, or as no text at all.
/// The text is taken, not copied, when the JSON read is no longer needed.
pub(crate) fn text_or_empty<'de, D: Deserializer<'de>>(sent: D) -> Result<String, D::Error> {
    struct TextOrNot;

    impl<'de> serde::de::Visitor<'de> for TextOrNot {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a text, or anything else")
        }

        fn visit_str<E>(self, text: &str) -> Result<String, E> {
            Ok(text.to_owned())
        }

        fn visit_string<E>(self, text: String) -> Result<String, E> {
            Ok(text)
        }

        otherwise!(String::new(); bool, number, null, list, record);
    }

    sent.deserialize_any(TextOrNot)
}

pub(crate) fn count_or_zero<'de, D: Deserializer<'de>>(sent: D) -> Result<u64, D::Error> {
    Ok(if_fits(sent)?.unwrap_or(0))
}

/// A payload that is a record or anything else. What is no record reads as
/// the record with every field left out; a record is read by its own rules.
#[derive(Default)]
pub(crate) struct RecordOrNot<T>(pub T);

impl<'de, T: Deserialize<'de> + Default> Deserialize<'de> for RecordOrNot<T> {
    fn deserialize<D: Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        struct Read<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de> + Default> serde::de::Visitor<'de> for Read<T> {
            type Value = T;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a record, or anything else")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, record: A) -> Result<T, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(record))
            }

            otherwise!(T::default(); bool, number, text, null, list);
        }

        sent.deserialize_any(Read(std::marker::PhantomData)).map(RecordOrNot)
    }
}

/// Reads a nullable field that is there: `Some(None)` for `null`. (A field
/// left out is `None`, by `#[serde(default)]`.)
fn sent<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(sent).map(Some)
}

/// Reads a nullable field that is there and may hold anything: `Some(None)`
/// for `null` and for whatever is no `T`.
fn sent_if_fits<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Option<Option<T>>, D::Error> {
    if_fits(sent).map(Some)
}

/// Reads a field that is a list of `T`, or anything else: `None` for what is
/// no list. A list must hold `T`s and nothing else.
pub(crate) fn list_if_list<'de, T: Deserialize<'de>, D: Deserializer<'de>>(sent: D) -> Result<Option<Vec<T>>, D::Error> {
    struct ListOrNot<T>(std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for ListOrNot<T> {
        type Value = Option<Vec<T>>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list, or anything else")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, list: A) -> Result<Self::Value, A::Error> {
            Vec::deserialize(serde::de::value::SeqAccessDeserializer::new(list)).map(Some)
        }

        otherwise!(None; bool, number, text, null, record);
    }

    sent.deserialize_any(ListOrNot(std::marker::PhantomData))
}

/// `ProjectMutation`: what `projects.mutate` is asked to do.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum ProjectMutation {
    #[serde(rename = "project.create", rename_all = "camelCase")]
    Create {
        command_id: String,
        project_id: String,
        title: String,
        workspace_root: String,
        #[serde(default)]
        create_workspace_root_if_missing: bool,
        #[serde(default)]
        default_model_selection: Option<ModelSelection>,
    },
    #[serde(rename = "project.update", rename_all = "camelCase")]
    Update {
        command_id: String,
        project_id: String,
        #[serde(flatten)]
        changes: ProjectChanges,
    },
    #[serde(rename = "project.delete", rename_all = "camelCase")]
    Delete { command_id: String, project_id: String },
}

/// What a `project.update` changes (`ProjectUpdatePayload`). A field left out
/// keeps its value, and so does a title, a folder, a flag or a list of scripts
/// sent as something else; `null` clears a nullable field, and a favicon path
/// or an env mode that is no text clears it too. A model selection, an icon
/// and each script must be the record it names.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChanges {
    #[serde(default, deserialize_with = "if_fits")]
    title: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    workspace_root: Option<String>,
    #[serde(default, deserialize_with = "sent")]
    default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, deserialize_with = "if_fits")]
    auto_pull: Option<bool>,
    #[serde(default, deserialize_with = "sent")]
    project_icon: Option<Option<ProjectIcon>>,
    #[serde(default, deserialize_with = "sent_if_fits")]
    favicon_path: Option<Option<String>>,
    #[serde(default, deserialize_with = "sent_if_fits")]
    default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(default, deserialize_with = "list_if_list")]
    scripts: Option<Vec<ProjectScript>>,
    #[serde(default, deserialize_with = "if_fits")]
    pinned: Option<bool>,
    #[serde(default, deserialize_with = "if_fits")]
    archived: Option<bool>,
}

/// Why `projects.mutate` changed nothing, as the client is told.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum MutationFailure {
    #[error("{message}")]
    #[serde(rename = "ProjectMutationError", rename_all = "camelCase")]
    Project { command_id: String, message: String },
}

fn mutation_error(command_id: &str, message: impl Into<String>) -> MutationFailure {
    MutationFailure::Project { command_id: command_id.to_owned(), message: message.into() }
}

pub fn apply_project_update(p: &mut ProjectShell, c: ProjectChanges) -> Result<(), String> {
    let now = || Some(crate::time_fmt::iso_now());
    if let Some(title) = c.title {
        if title.trim().is_empty() {
            return Err("Project title cannot be empty.".into());
        }
        p.title = title;
        p.has_custom_title = true;
    }
    if let Some(root) = c.workspace_root {
        if !std::path::Path::new(&root).is_absolute() {
            return Err("Workspace root must be an absolute path.".into());
        }
        p.workspace_root = root;
    }
    if let Some(selection) = c.default_model_selection {
        p.default_model_selection = selection;
    }
    if let Some(v) = c.auto_pull {
        p.auto_pull = v;
    }
    if let Some(icon) = c.project_icon {
        p.project_icon = icon;
    }
    if let Some(path) = c.favicon_path {
        p.favicon_path = path;
    }
    if let Some(mode) = c.default_thread_env_mode {
        p.default_thread_env_mode = mode.map(|mode| mode.as_str().to_owned());
    }
    if let Some(scripts) = c.scripts {
        p.scripts = scripts;
    }
    if let Some(v) = c.pinned {
        p.pinned_at = if v { p.pinned_at.clone().or_else(now) } else { None };
    }
    if let Some(v) = c.archived {
        p.archived_at = if v { p.archived_at.clone().or_else(now) } else { None };
    }
    Ok(())
}

async fn mutate(ctx: Arc<ConnCtx>, m: ProjectMutation) -> Result<ProjectRecord, MutationFailure> {
    let store = &ctx.state.store;
    match m {
        ProjectMutation::Create { command_id, project_id, title, workspace_root, create_workspace_root_if_missing, default_model_selection } => {
            let root = std::path::Path::new(&workspace_root);
            if !root.is_absolute() {
                return Err(mutation_error(&command_id, "Workspace root must be an absolute path."));
            }
            if !root.is_dir() {
                if !create_workspace_root_if_missing {
                    return Err(mutation_error(&command_id, format!("{workspace_root} does not exist.")));
                }
                std::fs::create_dir_all(root).map_err(|e| mutation_error(&command_id, e.to_string()))?;
            }
            let now = crate::time_fmt::iso_now();
            let shell = ProjectShell {
                id: project_id,
                title,
                has_custom_title: false,
                repository_identity: vcs::repository_identity(&workspace_root).await,
                workspace_root,
                default_model_selection,
                default_thread_env_mode: None,
                auto_pull: false,
                favicon_path: None,
                project_icon: None,
                scripts: vec![],
                pinned_at: None,
                archived_at: None,
                created_at: now.clone(),
                updated_at: now,
            };
            store.create_project(shell.clone(), Some(&command_id)).map_err(|e| mutation_error(&command_id, e.to_string()))?;
            Ok(ProjectRecord::from(&shell))
        }
        ProjectMutation::Update { command_id, project_id, changes } => {
            // Read and apply the patch under the writer so simultaneous icon and title edits compose.
            store.db().batch(|| -> Result<_, crate::store::StoreError> {
                let mut project = store.get_project(&project_id)?
                    .ok_or_else(|| crate::store::StoreError::Invalid("Unknown project.".into()))?;
                let reset_icon = changes.project_icon == Some(None) && changes.favicon_path == Some(None);
                apply_project_update(&mut project, changes).map_err(crate::store::StoreError::Invalid)?;
                if reset_icon && crate::managed::is_scratch_root(&project.workspace_root) {
                    project.project_icon = Some(crate::project_icons::scratch_default());
                }
                project.updated_at = crate::time_fmt::iso_now();
                store.update_project(project.clone(), Some(&command_id))?;
                Ok(ProjectRecord::from(&project))
            }).map_err(|e| mutation_error(&command_id, e.to_string()))
        }
        ProjectMutation::Delete { command_id, project_id } => {
            let project = store
                .get_project(&project_id)
                .map_err(|e| mutation_error(&command_id, e.to_string()))?
                .ok_or_else(|| mutation_error(&command_id, "Unknown project."))?;
            store.delete_project(&project_id, Some(&command_id)).map_err(|e| mutation_error(&command_id, e.to_string()))?;
            let mut record = ProjectRecord::from(&project);
            record.deleted_at = Some(crate::time_fmt::iso_now());
            Ok(record)
        }
    }
}

/// Commands only the server dispatches
/// that Domovoi carries out, so a client must not reach them.
const SERVER_ONLY_COMMANDS: [&str; 2] = ["secret_request.record", "link_request.record"];

/// The command a client sent, decoded: the one place its JSON is read. One
/// that is not a command (no id, no type, or a field its type does not
/// allow) is refused as a command is, so the client is told which.
fn decode_command(input: Value) -> Result<Command, DispatchFailure> {
    let named = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
    let (command_id, command_type) = (named("commandId"), named("type"));
    let refusal = match (&command_id, &command_type) {
        (None, _) => Refusal::NoCommandId,
        (Some(_), None) => Refusal::NoCommandType,
        // Server-only cards: no client may mark a secret saved without the value stored.
        (Some(_), Some(command_type)) if SERVER_ONLY_COMMANDS.contains(&command_type.as_str()) => Refusal::Unsupported(command_type.clone()),
        (Some(_), Some(command_type)) => match Command::from_value(input) {
            Ok(command) => return Ok(command),
            Err(e) => Refusal::Malformed { command_type: command_type.clone(), reason: e.to_string() },
        },
    };
    Err(DispatchFailure { command_id: command_id.map(Into::into), command_type, refusal })
}

/// `orchestration.dispatchCommand`, from the JSON a client sent.
pub(crate) async fn dispatch_command(orchestrator: &Orchestrator, input: Value) -> Result<DispatchResult, DispatchFailure> {
    orchestrator.dispatch(decode_command(input)?).await
}

/// `orchestration.launchThread`, from the JSON a client sent.
pub(crate) async fn launch_thread(orchestrator: &Orchestrator, input: Value) -> Result<LaunchResult, LaunchFailure> {
    let named = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
    let (command_id, project_id) = (named("commandId"), named("projectId"));
    let refused = |refusal: LaunchRefusal| LaunchFailure { command_id: command_id.clone().map(Into::into), project_id: project_id.clone().map(Into::into), refusal };
    if command_id.is_none() {
        return Err(refused(LaunchRefusal::NoCommandId));
    }
    let input: ThreadLaunchInput = serde_json::from_value(input).map_err(|e| refused(LaunchRefusal::Malformed(e.to_string())))?;
    orchestrator.launch_thread(input).await
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("orchestration.subscribeShell", |ctx, input: ShellSubscription| shell_stream(ctx, input))
        .unary("projects.mutate", mutate)
        .unary("orchestration.launchThread", |ctx: Arc<ConnCtx>, input: Value| async move { launch_thread(&ctx.state.orchestrator, input).await })
        .unary("orchestration.dispatchCommand", |ctx: Arc<ConnCtx>, input: Value| async move { dispatch_command(&ctx.state.orchestrator, input).await })
        .unary("orchestration.getTurnDiff", |ctx: Arc<ConnCtx>, input: TurnDiffInput| async move {
            crate::orchestration::checkpoints::turn_diff(&ctx.state.store, &input.thread_id, input.from_turn_count, input.to_turn_count, DiffCall::TurnDiff).await
        })
        .unary("orchestration.getFullThreadDiff", |ctx: Arc<ConnCtx>, input: TurnDiffInput| async move {
            crate::orchestration::checkpoints::turn_diff(&ctx.state.store, &input.thread_id, 0, input.to_turn_count, DiffCall::FullThreadDiff).await
        })
        .stream("orchestration.subscribeThread", |ctx, input: crate::orchestration::stream::ThreadSubscription| {
            crate::orchestration::stream::thread_stream(ctx.state.store.clone(), input, false)
        })
        .stream("orchestration.subscribeThreadWindow", |ctx, mut input: crate::orchestration::stream::ThreadSubscription| {
            input.all_records = false;
            crate::orchestration::stream::thread_stream(ctx.state.store.clone(), input, true)
        })
        .unary("orchestration.getThreadHistory", |ctx: Arc<ConnCtx>, input: crate::orchestration::history::HistoryInput| async move {
            crate::orchestration::history::read_older(ctx.state.store.clone(), input).await
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::events::Origin;
    use crate::store::Store;
    use crate::test_records::{append, message, thread};
    use domovoi_model::typed::Timestamp;

    #[test]
    fn shells_that_differ_only_in_when_they_changed_are_the_same_to_the_stream() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        append(&store, "t1", "thread.created", thread("t1", "p"), &Origin::default()).unwrap();
        let first = store.thread_shell("t1").unwrap().unwrap();
        // A streamed flush: the thread's `updatedAt` moves and nothing a list shows does.
        append(&store, "t1", "message.updated", message("t1", "m1", "assistant", "so far", true), &Origin::default()).unwrap();
        let second = store.thread_shell("t1").unwrap().unwrap();
        assert_ne!(first.updated_at, second.updated_at);
        assert!(same_apart_from_updated_at(&first, &second));
        let mut renamed = (*second).clone();
        renamed.title = "Another".into();
        assert!(!same_apart_from_updated_at(&second, &renamed));
        let mut later = (*second).clone();
        later.updated_at = Timestamp::from("2099-01-01T00:00:00.000Z");
        assert!(same_apart_from_updated_at(&second, &later) && *second != later);
    }

    #[test]
    fn a_snapshot_lists_each_thread_once_where_it_belongs() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        let archived = crate::test_records::with(thread("t2", "p"), serde_json::json!({ "archivedAt": crate::test_records::AT }));
        append(&store, "t2", "thread.created", archived, &o).unwrap();
        let deleted = crate::test_records::with(thread("t3", "p"), serde_json::json!({ "deletedAt": crate::test_records::AT }));
        append(&store, "t3", "thread.created", deleted, &o).unwrap();
        let snapshot = snapshot_of(&store).unwrap();
        let ids = |shells: &[Arc<ThreadShell>]| shells.iter().map(|s| s.id.as_str().to_owned()).collect::<Vec<_>>();
        assert_eq!((ids(&snapshot.threads), ids(&snapshot.archived_threads)), (vec!["t1".to_owned()], vec!["t2".to_owned()]));
        assert_eq!(serde_json::to_value(&snapshot).unwrap()["threads"][0], serde_json::to_value(&*snapshot.threads[0]).unwrap());
    }

    /// t3code `ShellStream.test.ts` "keeps thread rows out of the metadata-only enrichment frame".
    #[test]
    fn the_identity_item_repeats_no_thread_and_the_stream_snapshot_has_no_archive() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let o = Origin::default();
        store.create_project(project(), None).unwrap();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        let archived = crate::test_records::with(thread("t2", "p"), serde_json::json!({ "archivedAt": crate::test_records::AT }));
        append(&store, "t2", "thread.created", archived, &o).unwrap();
        let items = initial_items(snapshot_of(&store).unwrap());
        let sent: Vec<serde_json::Value> = items.iter().map(|i| serde_json::to_value(i).unwrap()).collect();
        assert_eq!(sent.len(), 2);
        // The authoritative snapshot: the active threads only, and no identity roots.
        assert_eq!(sent[0]["kind"], "snapshot");
        assert!(sent[0].get("resolvedRepositoryIdentityRoots").is_none());
        assert_eq!(sent[0]["snapshot"]["threads"].as_array().unwrap().len(), 1);
        assert_eq!(sent[0]["snapshot"]["threads"][0]["id"], "t1");
        assert_eq!(sent[0]["snapshot"]["archivedThreads"], serde_json::json!([]));
        // The identity item: the projects and their roots, no thread rows.
        assert_eq!(sent[1]["resolvedRepositoryIdentityRoots"], serde_json::json!(["/w"]));
        assert_eq!(sent[1]["snapshot"]["threads"], serde_json::json!([]));
        assert_eq!(sent[1]["snapshot"]["archivedThreads"], serde_json::json!([]));
        assert_eq!(sent[1]["snapshot"]["projects"][0]["id"], "p");
        assert_eq!(sent[1]["snapshot"]["snapshotSequence"], sent[0]["snapshot"]["snapshotSequence"]);
        // A resumed stream's identity item has the same shape, read without any thread.
        let resumed = serde_json::to_value(identity_item(projects_snapshot(&store).unwrap()).unwrap()).unwrap();
        assert_eq!(resumed, sent[1]);
        // With no project there is no identity item.
        let empty = Store::new(crate::db::Db::open_in_memory().unwrap());
        assert_eq!(initial_items(snapshot_of(&empty).unwrap()).len(), 1);
        assert!(identity_item(projects_snapshot(&empty).unwrap()).is_none());
    }

    #[test]
    fn identity_roots_name_each_root_once() {
        let mut other = project();
        other.id = "q".into();
        assert_eq!(identity_roots(&[project(), other]), vec!["/w".to_owned()]);
    }

    #[test]
    fn a_refused_mutation_is_written_as_a_tagged_error() {
        assert_eq!(
            serde_json::json!(mutation_error("c1", "Unknown project.")),
            serde_json::json!({ "_tag": "ProjectMutationError", "commandId": "c1", "message": "Unknown project." })
        );
    }

    fn project() -> ProjectShell {
        let mut p = crate::rpc::projects::new_project_shell("p".into(), "calc".into(), "/w".into(), None);
        (p.created_at, p.updated_at) = (crate::test_records::AT.into(), crate::test_records::AT.into());
        p.favicon_path = Some("a.png".into());
        p.default_thread_env_mode = Some("worktree".into());
        p.scripts = serde_json::from_value(serde_json::json!([{ "id": "t", "name": "T", "command": "t", "icon": "test", "runOnWorktreeCreate": false }])).unwrap();
        p
    }

    #[cfg(unix)]
    fn changes(sent: serde_json::Value) -> serde_json::Result<ProjectChanges> {
        match serde_json::from_value(sent)? {
            ProjectMutation::Update { changes, .. } => Ok(changes),
            _ => panic!("an update"),
        }
    }

    #[cfg(unix)]
    fn update(fields: serde_json::Value) -> serde_json::Value {
        let mut sent = serde_json::json!({ "type": "project.update", "commandId": "c", "projectId": "p" });
        sent.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        sent
    }

    // The workspace roots are Unix paths.
    #[cfg(unix)]
    #[test]
    fn an_update_keeps_what_it_does_not_name_and_passes_over_what_is_not_of_its_type() {
        // Nothing named: nothing changes.
        let mut p = project();
        apply_project_update(&mut p, changes(update(serde_json::json!({}))).unwrap()).unwrap();
        assert_eq!(p, project());
        // Not of their types: passed over, but a favicon path and an env mode that are no text are cleared.
        let odd = serde_json::json!({ "title": 5, "workspaceRoot": 6, "autoPull": "yes", "faviconPath": 7, "defaultThreadEnvMode": 8, "scripts": "none", "pinned": null, "archived": 1 });
        apply_project_update(&mut p, changes(update(odd)).unwrap()).unwrap();
        let mut cleared = project();
        (cleared.favicon_path, cleared.default_thread_env_mode) = (None, None);
        assert_eq!(p, cleared);
        // Named and of their types: stored as sent.
        let selection = serde_json::json!({ "instanceId": "codex", "model": "gpt-6-luna", "options": [{ "id": "effort", "value": "high" }, { "id": "fast", "value": true }] });
        let icon = serde_json::json!({ "kind": "monogram", "text": "PT", "color": "teal" });
        let scripts = serde_json::json!([
            { "id": "dev", "name": "Dev", "command": "npm run dev", "icon": "play", "runOnWorktreeCreate": false, "runOnSettle": false, "stopOnSettle": true, "async": true, "previewUrl": "http://localhost:3000", "autoOpenPreview": true },
            { "id": "t", "name": "T", "command": "t", "icon": "a-new-icon", "runOnWorktreeCreate": true },
        ]);
        let named = serde_json::json!({ "title": "Calc", "workspaceRoot": "/x", "defaultModelSelection": selection, "autoPull": true, "projectIcon": icon,
            "faviconPath": "b.png", "defaultThreadEnvMode": "local", "scripts": scripts, "pinned": true, "archived": true });
        apply_project_update(&mut p, changes(update(named)).unwrap()).unwrap();
        assert_eq!((p.title.as_str(), p.has_custom_title, p.workspace_root.as_str(), p.auto_pull), ("Calc", true, "/x", true));
        assert_eq!((serde_json::json!(p.default_model_selection), serde_json::json!(p.project_icon)), (selection, icon));
        assert_eq!((p.favicon_path.as_deref(), p.default_thread_env_mode.as_deref()), (Some("b.png"), Some("local")));
        assert_eq!(serde_json::json!(p.scripts), scripts);
        assert!(p.pinned_at.is_some() && p.archived_at.is_some());
        // An icon of a kind this build does not know is kept whole; `null` clears.
        let unknown = serde_json::json!({ "kind": "sticker", "pack": "cats", "index": 3 });
        apply_project_update(&mut p, changes(update(serde_json::json!({ "projectIcon": unknown, "defaultModelSelection": null, "scripts": [], "pinned": false }))).unwrap()).unwrap();
        assert_eq!((serde_json::json!(p.project_icon), p.default_model_selection.as_ref(), p.scripts.len(), p.pinned_at.as_ref()), (unknown, None, 0, None));
        apply_project_update(&mut p, changes(update(serde_json::json!({ "projectIcon": null }))).unwrap()).unwrap();
        assert_eq!(p.project_icon, None);
        // What is refused, and why.
        let refused = |fields: serde_json::Value| apply_project_update(&mut project(), changes(update(fields)).unwrap()).unwrap_err();
        assert_eq!(refused(serde_json::json!({ "title": "  " })), "Project title cannot be empty.");
        assert_eq!(refused(serde_json::json!({ "workspaceRoot": "relative" })), "Workspace root must be an absolute path.");
        // A selection, an icon or a script that is not one is no update at all.
        for malformed in [
            serde_json::json!({ "defaultModelSelection": { "model": "no instance" } }),
            serde_json::json!({ "projectIcon": { "kind": "lucide" } }),
            serde_json::json!({ "projectIcon": "star" }),
            serde_json::json!({ "scripts": [{ "id": "only" }] }),
            serde_json::json!({ "scripts": [5] }),
        ] {
            assert!(changes(update(malformed.clone())).is_err(), "{malformed}");
        }
    }

    #[test]
    fn fields_that_may_hold_anything_read_as_their_type_or_not_at_all() {
        #[derive(Deserialize)]
        struct Sent {
            #[serde(default, deserialize_with = "text_or_empty")]
            text: String,
            #[serde(default, deserialize_with = "count_or_zero")]
            count: u64,
            #[serde(default, deserialize_with = "if_fits")]
            flag: Option<bool>,
            #[serde(default, deserialize_with = "those_that_fit")]
            names: Vec<String>,
        }
        let read = |sent: serde_json::Value| {
            let sent: Sent = serde_json::from_value(sent).unwrap();
            (sent.text, sent.count, sent.flag, sent.names)
        };
        assert_eq!(read(serde_json::json!({})), (String::new(), 0, None, vec![]));
        assert_eq!(read(serde_json::json!({ "text": "a", "count": 3, "flag": false, "names": ["x", 5, null, "y", ["z"]] })), ("a".into(), 3, Some(false), vec!["x".to_owned(), "y".to_owned()]));
        assert_eq!(read(serde_json::json!({ "text": 5, "count": -1, "flag": "yes", "names": "x" })), (String::new(), 0, None, vec![]));
        assert_eq!(read(serde_json::json!({ "text": null, "count": 3.0, "flag": null, "names": { "x": 1 } })), (String::new(), 0, None, vec![]));
        assert_eq!(read(serde_json::json!({ "text": ["a"], "count": "3", "flag": 1, "names": null })), (String::new(), 0, None, vec![]));
        // A whole payload that may be anything: a record by its own rules, anything else as one with nothing in it.
        #[derive(Deserialize, Default)]
        struct Whole {
            #[serde(default, deserialize_with = "text_or_empty")]
            text: String,
            #[serde(default)]
            strict: Option<bool>,
        }
        let whole = |sent: serde_json::Value| serde_json::from_value::<RecordOrNot<Whole>>(sent).map(|read| (read.0.text, read.0.strict));
        assert_eq!(whole(serde_json::json!({ "text": "a", "strict": true })).unwrap(), ("a".to_owned(), Some(true)));
        for other in [serde_json::json!(null), serde_json::json!("a"), serde_json::json!(7), serde_json::json!(true), serde_json::json!(1.5), serde_json::json!(["a", true]), serde_json::json!([])] {
            assert_eq!(whole(other.clone()).unwrap(), (String::new(), None), "{other}");
        }
        assert!(whole(serde_json::json!({ "strict": "yes" })).is_err(), "a field with rules keeps them");
        // From text too, where nothing is parsed ahead.
        let read: RecordOrNot<Whole> = serde_json::from_str(r#"[1, {"a": [2]}, "x"]"#).unwrap();
        assert_eq!(read.0.text, "");
        let read: Sent = serde_json::from_str(r#"{ "text": {"a": [1, 2]}, "names": ["x", {"y": 1}, "z"], "count": 2 }"#).unwrap();
        assert_eq!((read.text.as_str(), read.names, read.count), ("", vec!["x".to_owned(), "z".to_owned()], 2));
    }
}

