//! What is kept on the device for a server: the drafts typed and the thread
//! that was open, and an offline copy of the thread list and the open
//! thread, shown at a launch until the server answers.
//!
//! This is the edge where the core's state is written to the shell's store
//! and read back from it. The open thread is saved as its projection, the
//! form the server sends it in, and read back the way a snapshot from the
//! server is (`replies::thread_from_projection`).

#![expect(clippy::disallowed_types, reason = "the offline copy: the saved thread's projection is read back the way a snapshot from the server is")]

use crate::environments::store;
use crate::model::Model;
use crate::store::StoreOperation;
use crate::update::Cmd;
use crux_core::{Command, render::render};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use domovoi_model::typed::{ProjectShell, ThreadProjection, ThreadShell};

pub fn drafts_key(environment_id: &str) -> String {
    format!("drafts:{environment_id}")
}

/// Saves the active server's drafts and open thread, so they outlive the
/// app (iOS ends it in the background).
pub fn save_drafts(model: &mut Model) -> Cmd {
    let Some(id) = model.active_env.clone() else { return Command::done() };
    model.drafts_dirty = false;
    let drafts: BTreeMap<&str, &str> = model.composer.drafts.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let thread = if model.route == "thread" { model.selected_thread.as_deref() } else { None };
    let stash = model.composer.stash.iter().map(String::as_str).collect();
    let kept = KeptDrafts { drafts, thread, stash };
    store(StoreOperation::Save { key: drafts_key(&id), value: serde_json::to_string(&kept).expect("plain data serializes") })
}

/// What `save_drafts` saves: the drafts by where they were typed, and the
/// thread that was open (`null` when none was).
#[derive(Serialize)]
struct KeptDrafts<'a> {
    drafts: BTreeMap<&'a str, &'a str>,
    thread: Option<&'a str>,
    stash: Vec<&'a str>,
}

/// Saved drafts are back: what isn't typed already goes in, and the thread
/// that was open opens again (once connected).
pub fn drafts_loaded(model: &mut Model, environment_id: String, value: Option<String>) -> Cmd {
    if model.active_env.as_deref() != Some(environment_id.as_str()) {
        return Command::done();
    }
    let Some(saved) = value.and_then(|v| serde_json::from_str::<SavedDrafts>(&v).ok()) else { return Command::done() };
    for (key, text) in saved.drafts {
        let current = model.composer.drafts.entry(key).or_default();
        if current.trim().is_empty() {
            *current = text;
        }
    }
    if model.composer.stash.is_empty() {
        model.composer.stash = saved.stash;
    }
    model.composer.draft_revision += 1;
    if model.selected_thread.is_none() && (model.route.is_empty() || model.route == "new-thread")
        && let Some(thread) = saved.thread
    {
        model.open_thread(Some(thread));
        model.route = "thread".into();
    }
    render()
}

/// What `save_drafts` saved.
#[derive(Deserialize)]
struct SavedDrafts {
    #[serde(default)]
    drafts: HashMap<String, String>,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    stash: Vec<String>,
}

/// How often a changed thread list is saved for offline use (and on suspend).
pub const CACHE_EVERY_MS: i64 = 120_000;
/// An open thread bigger than this isn't kept offline.
const CACHED_THREAD_MAX: usize = 2 * 1024 * 1024;

/// The offline copy's key (the shell keeps `cache:` keys in a file, not the Keychain).
pub fn cache_key(environment_id: &str) -> String {
    format!("cache:shell:{environment_id}")
}

/// What is saved for a launch without the server.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OfflineCopy<'a> {
    version: u32,
    saved_at: i64,
    projects: &'a [ProjectShell],
    threads: &'a [ThreadShell],
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<&'a ThreadProjection>,
}

/// An offline copy as it is read back: the thread list as the records the
/// server sent. A copy holding anything that is not such a record (one saved
/// by a version of this app that kept other shapes) does not read, and is
/// left alone until the next save replaces it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedCopy {
    version: u32,
    projects: Vec<ProjectShell>,
    threads: Vec<ThreadShell>,
    /// The open thread's projection, still as JSON: `thread_from_projection`
    /// takes it apart into the thread's records.
    #[serde(default)]
    thread: Option<Value>,
}

/// Saves the thread list and the open thread for a launch without the server.
pub fn save_cache(model: &mut Model) -> Cmd {
    let Some(id) = model.active_env.clone() else { return Command::done() };
    if model.threads.is_empty() || model.from_cache {
        return Command::done();
    }
    model.cache_dirty = false;
    model.cache_saved_ms = model.now_ms;
    // The open thread goes in as its projection, the form the server sends it
    // in, encoded once: its size decides whether it goes in at all. Only the
    // selected thread's own records go in, as the thread they are.
    let thread = model
        .selected_thread
        .as_deref()
        .and_then(|id| model.held_thread(id))
        .and_then(|thread| serde_json::to_string(&thread.projection()).ok())
        .filter(|text| text.len() <= CACHED_THREAD_MAX);
    let copy = OfflineCopy { version: 1, saved_at: model.now_ms, projects: &model.projects, threads: &model.threads, thread: None };
    let mut value = serde_json::to_string(&copy).expect("records and JSON serialize");
    if let Some(thread) = thread
        && value.ends_with('}')
    {
        value.pop();
        value.push_str(",\"thread\":");
        value.push_str(&thread);
        value.push('}');
    }
    store(StoreOperation::Save { key: cache_key(&id), value })
}

/// The offline copy is back: shown until the server's own list arrives.
pub fn cache_loaded(model: &mut Model, environment_id: String, value: Option<String>) -> Cmd {
    if model.active_env.as_deref() != Some(environment_id.as_str()) || !model.threads.is_empty() {
        return Command::done();
    }
    let Some(saved) = value.and_then(|v| serde_json::from_str::<SavedCopy>(&v).ok()).filter(|saved| saved.version == 1) else { return Command::done() };
    model.projects = saved.projects;
    model.threads = saved.threads;
    model.from_cache = !model.threads.is_empty();
    if model.selected_project.is_none() {
        model.selected_project = model.projects.first().map(|p| p.id.as_str().to_owned());
    }
    // A saved thread this app cannot read (saved by another version of it) is left out.
    if model.thread.is_none()
        && let Some(projection) = saved.thread
        && let Some(thread) = crate::replies::thread_from_projection(projection)
        && model.selected_thread.as_deref() == Some(thread.record.id.as_str())
    {
        model.thread = Some(thread);
    }
    render()
}
