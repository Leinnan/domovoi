//! Server replies: stream chunks and call results routed into the model.
//!
//! This is where what the server sends becomes records: each reply is read
//! once, as the record its RPC answers with, and one that does not fit is
//! left out and counted (`Model::left_out`), never kept as it came. What a
//! server answers over HTTP is read the same way, in [`http`].

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "what a server sends becomes records here: each reply and each HTTP answer is read once")]

use crate::model::{Connection, Model, Preview};
use crate::notify::NotifyOperation;
use crate::rpc::{Reply, Route, RpcClient, failure_message};
use crate::update::{self, Cmd, send};
use crux_core::{Command, render::render};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use domovoi_model::typed::{
    AgentSessionImportResult, AgentSessionScanResult, Applied, ArchivedShellSnapshot, AssetCreateUrlResult, AuthAccessSnapshot,
    AuthAccessStreamEvent, DatabaseStorageItem, EventBody, FilesystemBrowseResult, FindInThreadResult, ForwardList, GitActionProgressEvent,
    GitPreparePullRequestThreadResult, HostResourcesSnapshot, MessageRole, PersistChatAttachmentsResult, Project, ProjectCreateNewResult,
    ProjectEnsureScratchResult, ProjectEntriesResult, ProjectReadFileResult, ProjectShell, PullRequestChecksResult, PullRequestListResult,
    PullRequestSummary, RecordRef, ReviewDiffPreviewResult, RuntimeRequestKind, ScheduledTaskList, SearchThreadsResult,
    ServerAuthSessionMethod, ServerConfigStreamEvent, ServerLifecycleStreamEvent, ServerProcessDiagnosticsResult, ServerSettings,
    ServerUpsertKeybindingResult, ShellStatus, ShellStreamItem, ShellThreadLocation, StorageCleanupReportItem, TerminalAttachEvent,
    TerminalMetadataEvent, Thread, ThreadChange, ThreadShell, ThreadStreamItem, ThreadTurnDiff, TurnItem, UsageSummary, VcsListRefsResult,
    VcsPullResult, VcsPullStatus, VcsStatusStreamEvent, notice,
};

/// The sidebar status of a thread shell.
pub use domovoi_model::typed::shell_status;

/// What the server sent, read as the record `T`; one that does not fit is
/// left out, counted as `what`.
fn read<T: DeserializeOwned>(model: &mut Model, what: &'static str, sent: Value) -> Option<T> {
    match serde_json::from_value(sent) {
        Ok(record) => Some(record),
        Err(why) => {
            model.left_out(what, &why);
            None
        }
    }
}

/// The elements of a list the server may put newer kinds of element in:
/// the ones this build cannot read are counted as `what`. (A `null` is the
/// server's own mark for an element it left out.)
fn listed<T>(model: &mut Model, what: &'static str, list: ForwardList<T>) -> Vec<T> {
    let (items, unread) = list.into_parts();
    for _ in unread.iter().filter(|sent| !sent.is_null()) {
        model.left_out(what, &"an element of a kind this version does not know");
    }
    items
}

/// A notification when a thread starts needing the user, unless they are looking at it.
fn notify_transition(model: &mut Model, shell: &ThreadShell) -> Option<Cmd> {
    let id = shell.id.as_str().to_owned();
    let status = shell_status(shell);
    let previous = model.notified.insert(id.clone(), status);
    if previous == Some(status) || previous.is_none() {
        return None;
    }
    let watching = model.focused && model.selected_thread.as_deref() == Some(id.as_str());
    let body = notice(previous, status)?;
    if watching {
        return None;
    }
    let title = shell.title.clone();
    // An approval (not a question, which needs the app) can be answered from the notification.
    let request_id = shell.pending_runtime_request.as_ref().filter(|pending| status == ShellStatus::Approval && pending.kind != RuntimeRequestKind::UserInput).map(|pending| pending.id.as_str().to_owned());
    // The server's own id (what its pushes carry and `RespondFromNotification` checks).
    let environment_id = model.environment.as_ref().map(|environment| environment.environment_id.as_str().to_owned())
        .or_else(|| model.active_environment().map(|e| e.server_id.clone()))
        .unwrap_or_default();
    Some(Command::request_from_shell(NotifyOperation { title, body: body.into(), thread_id: id, environment_id, request_id }).then_send(|_| crate::Event::Notified))
}

fn upsert_project(list: &mut Vec<ProjectShell>, project: ProjectShell) {
    match list.iter_mut().find(|held| held.id == project.id) {
        Some(held) => *held = project,
        None => list.push(project),
    }
}

fn upsert_thread(list: &mut Vec<ThreadShell>, thread: ThreadShell) {
    match list.iter_mut().find(|held| held.id == thread.id) {
        Some(held) => *held = thread,
        None => list.push(thread),
    }
}

fn shell_chunk(model: &mut Model, item: Value) -> Vec<Cmd> {
    let mut cmds = vec![];
    // Where the item is in the server's log, whether or not it can be read.
    if let Some(seq) = item["sequence"].as_u64().or_else(|| item["snapshot"]["snapshotSequence"].as_u64()) {
        model.shell_sequence = Some(model.shell_sequence.map_or(seq, |s| s.max(seq)));
    }
    let item = match ShellStreamItem::from_value(item) {
        Ok(item) => item,
        Err(why) => {
            model.left_out("part of the thread list", &why);
            return cmds;
        }
    };
    match item {
        // Identity-roots snapshots only refresh project identities (t3code ShellStream).
        ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: Some(_) } => {
            for project in snapshot.projects {
                upsert_project(&mut model.projects, project);
            }
        }
        ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: None } => {
            model.from_cache = false;
            model.cache_dirty = true;
            model.projects = snapshot.projects;
            model.threads = snapshot.threads;
            // Where each thread stands now, so only a change from here notifies.
            for thread in &model.threads {
                model.notified.insert(thread.id.as_str().to_owned(), shell_status(thread));
            }
            if model.selected_project.is_none() {
                model.selected_project = model.projects.first().map(|p| p.id.as_str().to_owned());
            }
            if model.route.is_empty() {
                model.route = "new-thread".into();
                cmds.push(update::refresh_context(model));
            }
        }
        ShellStreamItem::ProjectUpdated { project, .. } => {
            model.cache_dirty = true;
            upsert_project(&mut model.projects, *project);
        }
        ShellStreamItem::ProjectRemoved { project_id, .. } => model.projects.retain(|p| p.id != project_id),
        ShellStreamItem::ThreadUpdated { location, thread, .. } => {
            model.cache_dirty = true;
            if location == ShellThreadLocation::Archive {
                model.threads.retain(|held| held.id != thread.id);
                upsert_thread(&mut model.archived, *thread);
            } else {
                model.archived.retain(|held| held.id != thread.id);
                if let Some(c) = notify_transition(model, &thread) {
                    cmds.push(c);
                }
                let open = model.open_when_created.as_deref() == Some(thread.id.as_str());
                upsert_thread(&mut model.threads, *thread);
                if open && let Some(id) = model.open_when_created.take() {
                    cmds.push(update::select_thread(model, id));
                }
            }
        }
        ShellStreamItem::ThreadRemoved { thread_id, .. } => {
            model.threads.retain(|held| held.id != thread_id);
            model.archived.retain(|held| held.id != thread_id);
        }
        ShellStreamItem::Synchronized | ShellStreamItem::SnapshotRequired | ShellStreamItem::Other(_) => {}
    }
    cmds
}

/// What a thread event changed on screen.
enum Reach {
    /// Nothing that is shown.
    Nothing,
    /// The text of one row, by turn item id.
    Row(String),
    /// More than that, or it cannot be told.
    Everything,
}

/// A reply streaming into the thread is the same records again with more
/// text: the event then reaches its row and nothing else. Any other event,
/// and any other difference (a status, a new record, an order), may change
/// which rows show, how they group and what the page says around them.
///
/// `applied` is what the event did to `thread`, which already holds its record.
fn reach(thread: &Thread, applied: &Applied) -> Reach {
    match applied {
        // Execution nodes are not shown.
        Applied::Added { record: RecordRef::Node(_), .. } | Applied::Replaced { record: RecordRef::Node(_), .. } => Reach::Nothing,
        Applied::Replaced { record: RecordRef::Message(id), only_text_grew: true, .. }
            if thread.messages.get(id).is_some_and(|message| message.role == MessageRole::Assistant) =>
        {
            match thread.turn_item_of_message(id) {
                Some(item) => Reach::Row(item.id().as_str().to_owned()),
                None => Reach::Everything,
            }
        }
        Applied::Replaced { record: RecordRef::TurnItem(id), only_text_grew: true, .. }
            if matches!(thread.turn_items.get(id), Some(TurnItem::AssistantMessage(_) | TurnItem::Reasoning(_) | TurnItem::CommandExecution(_))) =>
        {
            Reach::Row(id.as_str().to_owned())
        }
        _ => Reach::Everything,
    }
}

/// domovoi-server writes the patch of a Codex edit on its `file_change` item as
/// `diff`. The schema's field for an edit's diff is `diffStr`, the one a
/// thread keeps: the patch is moved there.
fn name_patches_as_the_schema_does(item: &mut Value) {
    fn rename(turn_item: &mut Value) {
        if turn_item["type"] == "file_change"
            && let Some(fields) = turn_item.as_object_mut()
            && !fields.contains_key("diffStr")
            && let Some(patch) = fields.remove("diff")
        {
            fields.insert("diffStr".into(), patch);
        }
    }
    if item["kind"] == "event" {
        if item["event"]["type"] == "turn-item.updated"
            && let Some(turn_item) = item.get_mut("event").and_then(|event| event.get_mut("payload"))
        {
            rename(turn_item);
        }
        return;
    }
    let Some(projection) = item.get_mut("projection") else { return };
    for turn_item in projection.get_mut("turnItems").and_then(Value::as_array_mut).into_iter().flatten() {
        rename(turn_item);
    }
    for visible in projection.get_mut("visibleTurnItems").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(turn_item) = visible.get_mut("item") {
            rename(turn_item);
        }
    }
}

/// One item of a thread's stream, read from the JSON it arrived as.
fn stream_item(mut item: Value) -> serde_json::Result<ThreadStreamItem> {
    name_patches_as_the_schema_does(&mut item);
    ThreadStreamItem::from_value(item)
}

/// The thread a stored projection describes (the offline copy of the open
/// thread), or `None` if it is not one this app can read.
pub(crate) fn thread_from_projection(projection: Value) -> Option<Thread> {
    match stream_item(json!({ "kind": "snapshot", "snapshotSequence": 0, "projection": projection })) {
        Ok(ThreadStreamItem::Snapshot(snapshot)) => Some(Thread::from_snapshot(*snapshot)),
        _ => None,
    }
}

/// An item of the open thread's stream that this app cannot read (a record
/// that does not fit the schema this build knows) is left out: the thread
/// stays as it was, the stream goes on from after the item, and the user is
/// told, once for a thread, that what is shown may be incomplete.
fn left_out(model: &mut Model, sequence: Option<u64>, why: &serde_json::Error) {
    model.changes.frame_is_wide();
    if let Some(sequence) = sequence {
        model.thread_sequence = model.thread_sequence.max(sequence);
    }
    model.thread_gaps += 1;
    if model.thread_gaps == 1 {
        let what = if model.thread.is_some() { "part of this thread, so what it shows may be incomplete" } else { "this thread" };
        model.toast("error", format!("This app can't read {what} ({why})."));
    }
}

fn thread_chunk(model: &mut Model, tid: &str, item: Value) -> Vec<Cmd> {
    if model.selected_thread.as_deref() != Some(tid) {
        return vec![];
    }
    // Where the item is in the server's log, should it turn out unreadable.
    let sequence = item["sequence"].as_u64().or_else(|| item["snapshotSequence"].as_u64());
    let item = match stream_item(item) {
        Ok(item) => item,
        Err(why) => {
            left_out(model, sequence, &why);
            return vec![];
        }
    };
    match item {
        // A snapshot replaces the thread.
        ThreadStreamItem::Snapshot(snapshot) => {
            model.changes.frame_is_wide();
            model.cache_dirty = true;
            model.thread_sequence = snapshot.snapshot_sequence;
            model.thread = Some(Thread::from_snapshot(*snapshot));
            model.thread_gaps = 0;
            model.thread_loading = false;
            // Rows are kept by the records they were built from, and every
            // record is a new one: none of them, or of what was worked out
            // for them, is of use again.
            model.item_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            model.markdown_prefixes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            let urls = update::request_asset_urls(model);
            if model.panel.open && model.panel.tab == "diff" {
                return vec![urls, update::load_diff(model)];
            }
            vec![urls]
        }
        ThreadStreamItem::Synchronized => {
            model.changes.frame_is_wide();
            model.thread_loading = false;
            vec![]
        }
        ThreadStreamItem::Event { sequence, event } => {
            // What the event asks for besides its record, read before the
            // record goes into the thread.
            let attachments = matches!(&event.body, EventBody::MessageUpdated(message) if !message.attachments.is_empty());
            let checkpoint = matches!(event.body, EventBody::CheckpointCaptured(_));
            let moved = matches!(event.body, EventBody::Thread { change: ThreadChange::MetadataUpdated, .. });
            // Only an event's reach is judged.
            if let Some(thread) = model.thread.as_mut() {
                let applied = thread.apply(*event);
                match reach(thread, &applied) {
                    Reach::Nothing => {}
                    Reach::Row(item) => model.changes.row(&item),
                    Reach::Everything => model.changes.frame_is_wide(),
                }
            }
            model.thread_sequence = model.thread_sequence.max(sequence);
            if attachments {
                return vec![update::request_asset_urls(model)];
            }
            if checkpoint && model.panel.open && model.panel.tab == "diff" {
                return vec![update::load_diff(model)];
            }
            // The thread moved to a worktree or another project.
            if moved {
                return vec![update::refresh_context(model)];
            }
            vec![]
        }
    }
}

/// What a git action's progress says, as a line for its log.
fn git_progress_line(event: &GitActionProgressEvent) -> Option<String> {
    Some(match event {
        GitActionProgressEvent::PhaseStarted(phase) => format!("{}…", phase.label),
        GitActionProgressEvent::HookStarted(hook) => format!("Running hook {}", hook.hook_name),
        GitActionProgressEvent::HookOutput(output) => output.text.trim_end().to_owned(),
        GitActionProgressEvent::ActionFailed(failed) => format!("Failed: {}", failed.message),
        GitActionProgressEvent::ActionFinished(_) => "Done".into(),
        GitActionProgressEvent::ActionStarted(_) | GitActionProgressEvent::HookFinished(_) | GitActionProgressEvent::Other(_) => return None,
    })
}

/// One change to who is signed in (`subscribeAuthAccess`).
fn access_changed(model: &mut Model, event: AuthAccessStreamEvent) {
    fn lists(model: &mut Model) -> &mut AuthAccessSnapshot {
        model.pages.access.get_or_insert_with(|| AuthAccessSnapshot { pairing_links: vec![], client_sessions: vec![] })
    }
    match event {
        AuthAccessStreamEvent::Snapshot(snapshot) => model.pages.access = Some(snapshot.payload),
        AuthAccessStreamEvent::PairingLinkUpserted(link) => {
            let links = &mut lists(model).pairing_links;
            links.retain(|held| held.id != link.payload.id);
            links.push(link.payload);
        }
        AuthAccessStreamEvent::PairingLinkRemoved(removed) => lists(model).pairing_links.retain(|held| held.id != removed.payload.id),
        AuthAccessStreamEvent::ClientUpserted(client) => {
            let client = client.payload;
            // A device just paired over iroh: the code on screen is spent.
            let new_device = !lists(model).client_sessions.iter().any(|held| held.session_id == client.session_id);
            if new_device && model.pages.remote_link.is_some() && client.method == ServerAuthSessionMethod::BearerAccessToken {
                model.pages.remote_link = None;
                let label = client.client.label.as_deref().unwrap_or("A device").to_owned();
                model.toast("success", format!("{label} paired."));
            }
            let sessions = &mut lists(model).client_sessions;
            sessions.retain(|held| held.session_id != client.session_id);
            sessions.push(client);
        }
        AuthAccessStreamEvent::ClientRemoved(removed) => lists(model).client_sessions.retain(|held| held.session_id != removed.payload.session_id),
        AuthAccessStreamEvent::Other(_) => {}
    }
}

fn chunk(model: &mut Model, route: &Route, v: Value) -> Vec<Cmd> {
    match route {
        // The open thread's stream says how far each of its events reaches.
        Route::Thread(tid) => thread_chunk(model, tid, v),
        Route::Config => {
            match read(model, "the server's settings", v) {
                Some(ServerConfigStreamEvent::Snapshot(snapshot)) => {
                    let config = snapshot.config;
                    model.set_settings(Some(config.settings));
                    model.providers = listed(model, "a provider", config.providers);
                    model.keybindings = listed(model, "a keyboard shortcut", config.keybindings);
                }
                Some(ServerConfigStreamEvent::ProviderStatuses(event)) => model.providers = listed(model, "a provider", event.payload.providers),
                Some(ServerConfigStreamEvent::SettingsUpdated(event)) => model.set_settings(Some(event.payload.settings)),
                Some(ServerConfigStreamEvent::KeybindingsUpdated(event)) => model.keybindings = listed(model, "a keyboard shortcut", event.payload.keybindings),
                _ => {}
            }
            vec![]
        }
        Route::Lifecycle => {
            if let Some(ServerLifecycleStreamEvent::Welcome(welcome)) = read(model, "the server's welcome", v) {
                model.environment = Some(welcome.payload.environment);
                return vec![crate::environments::on_welcome(model)];
            }
            vec![]
        }
        Route::Shell => shell_chunk(model, v),
        Route::VcsStatus(cwd) => {
            if &model.panel.git.cwd == cwd {
                match read(model, "the checkout's git status", v) {
                    Some(VcsStatusStreamEvent::Snapshot(status)) => {
                        model.panel.git.local = Some(status.local);
                        model.panel.git.remote = status.remote;
                    }
                    Some(VcsStatusStreamEvent::LocalUpdated(status)) => model.panel.git.local = Some(status.local),
                    Some(VcsStatusStreamEvent::RemoteUpdated(status)) => model.panel.git.remote = status.remote,
                    Some(VcsStatusStreamEvent::Other(_)) | None => {}
                }
            }
            vec![]
        }
        Route::Terminal { thread_id, terminal_id } => {
            if model.panel.terminal_owner.as_deref() == Some(thread_id.as_str())
                && let Some(at) = model.panel.terminals.iter().position(|t| &t.id == terminal_id)
            {
                match TerminalAttachEvent::from_value(v) {
                    Ok(event) => model.panel.terminals[at].apply(&event),
                    Err(why) => model.left_out("a terminal's output", &why),
                }
            }
            vec![]
        }
        Route::TerminalMetadata => {
            let list = &mut model.panel.terminal_summaries;
            match TerminalMetadataEvent::from_value(v) {
                Ok(TerminalMetadataEvent::Snapshot { terminals }) => *list = terminals,
                Ok(TerminalMetadataEvent::Remove { thread_id, terminal_id }) => list.retain(|t| !(t.thread_id == thread_id && t.terminal_id == terminal_id)),
                Ok(TerminalMetadataEvent::Upsert { terminal }) => {
                    list.retain(|t| !(t.thread_id == terminal.thread_id && t.terminal_id == terminal.terminal_id));
                    list.push(*terminal);
                }
                Ok(TerminalMetadataEvent::Other(_)) => {}
                Err(why) => model.left_out("the list of terminals", &why),
            }
            vec![update::adopt_terminals(model)]
        }
        Route::ScheduledTasks => {
            if let Some(list) = read::<ScheduledTaskList>(model, "the scheduled tasks", v) {
                model.pages.scheduled = listed(model, "a scheduled task", list.tasks);
                model.pages.scheduled_time_zone = list.time_zone.flatten().unwrap_or_default();
            }
            vec![]
        }
        Route::AuthAccess => {
            if let Some(event) = read(model, "who is signed in", v) {
                access_changed(model, event);
            }
            vec![]
        }
        Route::DatabaseStorage => {
            if let Some(status) = read::<DatabaseStorageItem>(model, "the database's size", v) {
                model.pages.database = status;
            }
            vec![]
        }
        Route::CleanupReport => {
            if let Some(report) = read::<StorageCleanupReportItem>(model, "the storage cleanup report", v) {
                model.pages.cleanup = report;
            }
            vec![]
        }
        Route::GitAction(_) => {
            let Some(event) = read::<GitActionProgressEvent>(model, "a git action's progress", v) else { return vec![] };
            if let Some(line) = git_progress_line(&event)
                && !line.is_empty()
            {
                model.panel.git.progress.push(line);
            }
            match event {
                GitActionProgressEvent::ActionFinished(finished) => {
                    model.panel.git.busy = false;
                    model.toast("success", finished.result.toast.title);
                }
                GitActionProgressEvent::ActionFailed(failed) => {
                    model.panel.git.busy = false;
                    model.panel.git.error = Some(failed.message);
                }
                _ => {}
            }
            vec![]
        }
        _ => vec![],
    }
}

/// The project a call that makes or finds one answered with (`projects.mutate`
/// answers with the project, `projects.createNew` and `projects.ensureScratch`
/// with its id).
fn project_made(model: &mut Model, sent: Value) -> Option<String> {
    if let Ok(made) = ProjectCreateNewResult::deserialize(&sent) {
        return Some(made.project_id.as_str().to_owned());
    }
    if let Ok(scratch) = ProjectEnsureScratchResult::deserialize(&sent) {
        return Some(scratch.project_id.as_str().to_owned());
    }
    read::<Project>(model, "the new project", sent).map(|project| project.id.as_str().to_owned())
}

/// Uploading a message's images did not end in attachments to send.
fn attachments_lost(model: &mut Model) {
    model.composer.sending = false;
    model.composer.pending_send = None;
    for a in model.composer.attachments.iter_mut() {
        a.uploading = false;
    }
}

fn exit_ok(model: &mut Model, route: Route, value: Value) -> Vec<Cmd> {
    match route {
        Route::Launch(tid) => {
            update::outbox_done(model, &Route::Launch(tid.clone()));
            model.composer.sending = false;
            vec![update::select_thread(model, tid)]
        }
        Route::Send(_) => {
            update::outbox_done(model, &route);
            vec![]
        }
        Route::ProjectCreated => {
            let id = project_made(model, value);
            vec![update::update(crate::Event::NewThread { project_id: id }, model)]
        }
        Route::AgentScan => {
            model.sessions.scanning = false;
            model.sessions.scanned = true;
            model.sessions.candidates = read::<AgentSessionScanResult>(model, "the sessions found", value).map(|scan| scan.candidates).unwrap_or_default();
            vec![]
        }
        Route::ImportProject { path, project_id } => {
            model.sessions.new_projects += 1;
            vec![update::import_sessions(model, &path, &project_id)]
        }
        Route::ImportSessions(_) => {
            model.sessions.imported_sessions += read::<AgentSessionImportResult>(model, "an import's result", value).map_or(0, |imported| imported.imported_count);
            vec![import_step_done(model)]
        }
        Route::Browse(rev) if rev == model.browser.revision => {
            let listing = read::<FilesystemBrowseResult>(model, "a folder's contents", value);
            let b = &mut model.browser;
            b.loading = false;
            let (folder, entries) = listing.map(|listing| (listing.parent_path, listing.entries)).unwrap_or_default();
            b.entries = entries;
            // The folder listed (`~` comes back expanded), and the one above it.
            if !folder.is_empty() {
                b.path = folder;
            }
            b.parent = match b.path.trim_end_matches('/').rsplit_once('/') {
                Some(("", _)) if b.path != "/" => "/".to_owned(),
                Some((up, _)) if !up.is_empty() => up.to_owned(),
                _ => String::new(),
            };
            vec![]
        }
        Route::SearchThreads(rev) if rev == model.search.revision => {
            model.search.results = read::<SearchThreadsResult>(model, "the search results", value).map(|found| found.matches).unwrap_or_default();
            model.search.searching = false;
            vec![]
        }
        Route::FindInThread(rev) if rev == model.find.revision => {
            model.find.result = read::<FindInThreadResult>(model, "what was found in the thread", value);
            vec![]
        }
        Route::Mention(rev) => {
            if model.composer.mention.as_ref().is_some_and(|m| m.revision == rev) {
                let entries = read::<ProjectEntriesResult>(model, "the files found", value).map(|found| found.entries).unwrap_or_default();
                if let Some(m) = model.composer.mention.as_mut() {
                    m.results = entries;
                    m.highlighted = 0;
                }
            }
            vec![]
        }
        Route::FileSearch(rev) if rev == model.panel.files.revision => {
            model.panel.files.results = read::<ProjectEntriesResult>(model, "the files found", value).map(|found| found.entries).unwrap_or_default();
            vec![]
        }
        Route::ListEntries(dir) => {
            model.panel.files.loading.remove(&dir);
            let entries = read::<ProjectEntriesResult>(model, "a folder's files", value).map(|listed| listed.entries).unwrap_or_default();
            model.panel.files.children.insert(dir, entries);
            vec![]
        }
        Route::AssetUrl(id) => {
            match read::<AssetCreateUrlResult>(model, "an attachment's address", value) {
                Some(asset) => {
                    model.asset_urls.insert(id, asset.relative_url);
                    vec![render()]
                }
                None => {
                    model.asset_urls.remove(&id);
                    vec![]
                }
            }
        }
        Route::ReadFile(path) => {
            if model.panel.files.preview.as_ref().is_some_and(|p| p.path == path) {
                model.panel.files.preview = Some(match read::<ProjectReadFileResult>(model, "a file", value) {
                    Some(file) => Preview {
                        spans: crate::highlight::highlight(&file.contents, &crate::highlight::language_for_path(&path)),
                        path,
                        text: file.contents,
                        truncated: file.truncated,
                        error: (file.exists == Some(false)).then(|| "The file does not exist.".into()),
                        loading: false,
                    },
                    None => Preview { path, error: Some("This app can't read the server's answer.".into()), ..Default::default() },
                });
            }
            vec![]
        }
        Route::Diff(source) if source == model.panel.diff.source => {
            // A thread's own diffs are one patch; a checkout's come by source.
            let (patch, truncated) = if source == "thread" || source.starts_with("turn:") {
                (read::<ThreadTurnDiff>(model, "a diff", value).map(|diff| diff.diff).unwrap_or_default(), false)
            } else {
                let first = read::<ReviewDiffPreviewResult>(model, "a diff", value).and_then(|preview| preview.sources.into_iter().next());
                first.map(|source| (source.diff, source.truncated)).unwrap_or_default()
            };
            let d = &mut model.panel.diff;
            d.loading = false;
            d.set_patch(&patch);
            d.truncated = truncated;
            vec![]
        }
        Route::ListRefs(cwd) => {
            if model.composer.branches_cwd == cwd {
                let refs = read::<VcsListRefsResult>(model, "the branches", value).map(|listed| listed.refs).unwrap_or_default();
                model.composer.branches = refs.into_iter().filter(|r| r.is_remote != Some(true)).map(|r| r.name).collect();
            }
            vec![]
        }
        Route::Attachments(ctx) => {
            let Some(saved) = read::<PersistChatAttachmentsResult>(model, "the attachments as saved", value) else {
                attachments_lost(model);
                return vec![];
            };
            let text = model.composer.pending_send.take().unwrap_or_default();
            let new_thread = ctx.get(2).is_some_and(|s| s == "true");
            let target = update::SendTarget::from_route(&ctx).unwrap_or_else(|| update::SendTarget::now(model));
            let [thread_id, message_id, ..] = ctx.as_slice() else {
                attachments_lost(model);
                return vec![];
            };
            vec![update::finish_send(model, thread_id, message_id, new_thread, text, saved.attachments, target)]
        }
        Route::Settings => {
            model.settings_before = None;
            // The settings as they are now; an answer without them leaves what is shown.
            if value.is_object()
                && let Some(settings) = read::<ServerSettings>(model, "the server's settings", value)
            {
                model.set_settings(Some(settings));
            }
            vec![]
        }
        Route::Keybindings => {
            let result = read::<ServerUpsertKeybindingResult>(model, "the keyboard shortcuts", value);
            model.keybindings = result.map(|result| listed(model, "a keyboard shortcut", result.keybindings)).unwrap_or_default();
            vec![]
        }
        Route::Diagnostics => {
            model.pages.diagnostics = read::<ServerProcessDiagnosticsResult>(model, "the server's processes", value);
            vec![]
        }
        Route::HostResources => {
            model.pages.host = read::<HostResourcesSnapshot>(model, "the server's resources", value);
            vec![]
        }
        Route::PullRequests => {
            model.pages.prs_loading = false;
            let list = read::<PullRequestListResult>(model, "the pull requests", value);
            let (entries, providers) = list.map(|list| (list.entries, list.providers)).unwrap_or_default();
            model.pages.prs = entries;
            model.pages.prs_notice = providers.into_iter().filter(|provider| !provider.configured).find_map(|provider| provider.detail);
            vec![]
        }
        Route::PullRequestDetail(key) if model.pages.pr_selected.as_deref() == Some(key.as_str()) => {
            model.pages.pr_detail = read::<PullRequestSummary>(model, "a pull request", value);
            vec![]
        }
        Route::PullRequestAction { key, done } => {
            model.pages.pr_busy = None;
            model.toast("info", done);
            let list = update::update(crate::app::Event::LoadPullRequests, model);
            let detail = if model.pages.pr_selected.as_deref() == Some(key.as_str()) { update::update(crate::app::Event::SelectPullRequest { key }, model) } else { Command::done() };
            vec![list, detail]
        }
        Route::PullRequestThread(key) => {
            model.pages.pr_busy = None;
            let entry = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key);
            let project = entry.map(|entry| entry.project_id.as_str().to_owned());
            let number = entry.map_or_else(|| "null".to_owned(), |entry| entry.number.to_string());
            let cmd = update::start_new_thread(model, project);
            if let Some(prepared) = read::<GitPreparePullRequestThreadResult>(model, "the pull request's worktree", value)
                && let Some(path) = prepared.worktree_path.filter(|path| !path.is_empty())
            {
                let label = format!("PR #{number} · {}", prepared.branch);
                model.composer.existing_worktree = Some((path, prepared.branch, label));
            }
            vec![cmd]
        }
        Route::PullRequestChecks(key) if model.pages.pr_selected.as_deref() == Some(key.as_str()) => {
            model.pages.pr_checks = read::<PullRequestChecksResult>(model, "a pull request's checks", value).flatten();
            vec![]
        }
        Route::Usage => {
            model.pages.usage_loading = false;
            let usage = read::<UsageSummary>(model, "the usage summary", value);
            // Rows of a kind this build does not know are not in the totals.
            for _ in usage.iter().flat_map(|usage| usage.buckets.left_out()).filter(|sent| !sent.is_null()) {
                model.left_out("a row of the usage summary", &"an element of a kind this version does not know");
            }
            model.pages.usage = usage;
            vec![]
        }
        Route::Archived => {
            model.archived = read::<ArchivedShellSnapshot>(model, "the archived threads", value).map(|snapshot| snapshot.threads).unwrap_or_default();
            vec![]
        }
        Route::Command(label) => {
            match label.as_str() {
                "Storage cleanup" | "Reclaim space" => {
                    model.pages.storage_busy = false;
                    model.toast("success", format!("{label} finished."));
                }
                "Pull" => {
                    model.panel.git.busy = false;
                    let up_to_date = read::<VcsPullResult>(model, "the pull's result", value).is_some_and(|pulled| pulled.status == VcsPullStatus::SkippedUpToDate);
                    model.toast("success", if up_to_date { "Already up to date." } else { "Pulled." });
                }
                "Make project" => model.toast("success", "The thread's folder is now a project."),
                "Create task" => model.toast("success", "Scheduled task created."),
                _ => {}
            }
            vec![]
        }
        _ => vec![],
    }
}

/// One folder of an import finished (or failed): the next one, or the summary.
fn import_step_done(model: &mut Model) -> Cmd {
    model.sessions.done += 1;
    if !model.sessions.importing() {
        let s = &model.sessions;
        let sessions = s.imported_sessions;
        let folders = s.total;
        let failures = s.failures.clone();
        let summary = match (sessions, folders) {
            (0, _) => "No sessions to import.".to_owned(),
            (1, 1) => "Imported 1 session.".to_owned(),
            (n, 1) => format!("Imported {n} sessions."),
            (n, f) => format!("Imported {n} sessions from {f} folders."),
        };
        if failures.is_empty() {
            model.toast("success", summary);
        } else {
            model.toast("error", format!("{summary} Not imported: {}", failures.join("; ")));
        }
        // The picker shows what is imported now.
        model.sessions.scanning = true;
        return update::call(model, crate::requests::agent_sessions_scan(), Route::AgentScan);
    }
    update::import_next(model)
}

fn exit_failed(model: &mut Model, route: Route, cause: &Value) -> Vec<Cmd> {
    let message = failure_message(cause);
    match &route {
        Route::AgentScan => {
            model.sessions.scanning = false;
            model.sessions.error = Some(message);
            return vec![];
        }
        Route::ImportProject { path, .. } | Route::ImportSessions(path) => {
            let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_owned();
            model.sessions.failures.push(format!("{name} ({message})"));
            return vec![import_step_done(model)];
        }
        Route::Browse(rev) => {
            if *rev == model.browser.revision {
                model.browser.loading = false;
                model.browser.error = Some(message);
            }
            return vec![];
        }
        Route::Launch(tid) => {
            let out = update::outbox_done(model, &route);
            model.composer.sending = false;
            // Sent again after a dropped connection, and the first one had
            // got as far as creating it: it is there to open.
            if message.contains("already exists") {
                return vec![update::select_thread(model, tid.clone())];
            }
            if let Some(out) = out {
                update::give_back(model, &out);
            }
        }
        Route::Send(_) => {
            if let Some(out) = update::outbox_done(model, &route) {
                update::give_back(model, &out);
                model.toast("error", format!("{}: {message}", out.label));
                return vec![];
            }
        }
        Route::Settings => {
            if let Some(before) = model.settings_before.take() {
                model.set_settings(before);
            }
        }
        Route::Attachments(_) => attachments_lost(model),
        Route::Diff(_) => {
            model.panel.diff.loading = false;
            model.panel.diff.set_patch("");
            model.panel.diff.error = Some(message);
            return vec![];
        }
        Route::ReadFile(path) => {
            if let Some(p) = model.panel.files.preview.as_mut().filter(|p| &p.path == path) {
                p.loading = false;
                p.error = Some(message);
            }
            return vec![];
        }
        Route::ListEntries(dir) => {
            model.panel.files.loading.remove(dir);
        }
        Route::PullRequests => {
            model.pages.prs_loading = false;
            model.pages.prs_error = Some(message);
            return vec![];
        }
        Route::Usage => model.pages.usage_loading = false,
        Route::Command(l) if l == "Pull" => model.panel.git.busy = false,
        Route::Command(l) if l == "Storage cleanup" || l == "Reclaim space" => model.pages.storage_busy = false,
        Route::GitAction(_) => model.panel.git.busy = false,
        Route::PullRequestAction { .. } | Route::PullRequestThread(_) => model.pages.pr_busy = None,
        Route::SearchThreads(_) => model.search.searching = false,
        // Errors on these are already visible where they happen, or harmless.
        // A later snapshot or message asks again.
        Route::AssetUrl(id) => {
            model.asset_urls.remove(id);
            return vec![];
        }
        Route::Mention(_) | Route::FileSearch(_) | Route::FindInThread(_) | Route::ListRefs(_) | Route::Archived => return vec![],
        Route::Thread(_) => model.thread_loading = false,
        _ => {}
    }
    let label = match &route {
        Route::Command(l) => l.clone(),
        Route::Launch(_) => "Start thread".into(),
        Route::Send(_) => "Send message".into(),
        Route::ProjectCreated => "Add project".into(),
        Route::Attachments(_) => "Attach images".into(),
        Route::PullRequestAction { .. } => "Pull request".into(),
        Route::PullRequestThread(_) => "Check out the pull request".into(),
        Route::Settings => "Save setting".into(),
        _ => "Request".into(),
    };
    model.toast("error", format!("{label}: {message}"));
    vec![]
}

/// Applies one reply. Renders nothing: the caller renders once for all the
/// replies a socket frame carried.
pub fn handle(model: &mut Model, reply: Reply) -> Cmd {
    match reply {
        Reply::Chunk { id, route, values } => {
            // The open thread's stream says how far each event reaches.
            if !matches!(route, Route::Thread(_)) {
                model.changes.frame_is_wide();
            }
            let mut cmds: Vec<Cmd> = values.into_iter().flat_map(|v| chunk(model, &route, v)).collect();
            // Acknowledge so the server sends the next batch.
            if model.connection == Connection::Connected {
                cmds.push(send(RpcClient::ack(&id)));
            }
            Command::all(cmds)
        }
        Reply::Exit { route, ok, value } => {
            model.changes.frame_is_wide();
            let cmds = if ok { exit_ok(model, route, value) } else { exit_failed(model, route, &value) };
            Command::all(cmds)
        }
        Reply::Defect(d) => {
            model.changes.frame_is_wide();
            model.toast("error", d.as_str().unwrap_or("Server error").to_owned());
            Command::done()
        }
    }
}


/// What a server answers over HTTP, read where it arrives.
pub mod http {
    use crate::model::Model;
    use serde::Deserialize;
    use serde::de::{DeserializeOwned, IgnoredAny};
    use serde_json::Value;
    use domovoi_model::typed::RequestRejection;

    pub type HttpResult = crux_http::Result<crux_http::Response<Vec<u8>>>;

    /// The body of an answer: the JSON it is, or `null` when it is none.
    pub struct Body(Value);

    impl Body {
        fn of(bytes: Option<&[u8]>) -> Self {
            Self(bytes.and_then(|bytes| serde_json::from_slice(bytes).ok()).unwrap_or(Value::Null))
        }

        /// The body as the record `T`, if it is one.
        pub fn read<T: DeserializeOwned>(&self) -> Option<T> {
            T::deserialize(&self.0).ok()
        }
    }

    /// An HTTP exchange's outcome: crux_http reports 4xx/5xx as errors.
    pub enum Answer {
        Ok(Body),
        Rejected(u16, Body),
        Unreachable(String),
    }

    pub fn answer(res: HttpResult) -> Answer {
        match res {
            Ok(r) if r.status().is_success() => Answer::Ok(Body::of(r.body().map(Vec::as_slice))),
            Ok(r) => Answer::Rejected(r.status().as_u16(), Body::of(r.body().map(Vec::as_slice))),
            Err(e) => match e.code() {
                Some(code) => Answer::Rejected(code, Body::of(e.body())),
                None => Answer::Unreachable(e.to_string()),
            },
        }
    }

    /// What the server says of a request it refused: its error's message or
    /// reason, or else the status it answered with.
    pub fn rejection_message(code: u16, body: &Body) -> String {
        /// An error body that is none of the API's tagged errors.
        #[derive(Deserialize)]
        struct Untagged {
            message: Option<String>,
            reason: Option<String>,
        }
        let said = match RequestRejection::deserialize(&body.0) {
            Ok(rejection) => rejection.message().or(rejection.reason()).map(str::to_owned),
            Err(_) => Untagged::deserialize(&body.0).ok().and_then(|said| said.message.or(said.reason)),
        };
        said.unwrap_or_else(|| format!("The server answered {code}."))
    }

    /// The body of an answer, read as the record `T`; one that does not fit
    /// is left out, counted as `what`.
    pub fn read<T: DeserializeOwned>(model: &mut Model, what: &'static str, body: Body) -> Option<T> {
        super::read(model, what, body.0)
    }

    /// Whether a refusal's body is the server's own error (a tagged error of
    /// its API, or an OAuth error) and not a proxy's or a captive portal's.
    pub fn is_servers_own(body: &Body) -> bool {
        #[derive(Deserialize)]
        struct Tagged {
            #[serde(rename = "_tag")]
            _tag: IgnoredAny,
        }
        #[derive(Deserialize)]
        struct OAuthError {
            #[serde(rename = "error")]
            _error: String,
        }
        Tagged::deserialize(&body.0).is_ok() || OAuthError::deserialize(&body.0).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thread shell as a server lists one (the first of the sample
    /// archived snapshot), with `given` over it.
    fn shell(given: Value) -> ThreadShell {
        let sample: Value = serde_json::from_str(include_str!("../../../crates/domovoi-model/tests/samples/orchestration.getArchivedShellSnapshot.json")).expect("the sample");
        let mut shell = sample["threads"][0].clone();
        for (key, value) in given.as_object().expect("fields").clone() {
            shell[key.as_str()] = value;
        }
        serde_json::from_value(shell).expect("the shell fits the schema")
    }

    #[test]
    fn shell_statuses() {
        let idle = json!({ "pendingRuntimeRequest": null, "activeRunId": null, "activityRunStatus": null });
        let with = |given: Value| {
            let mut fields = idle.clone();
            fields.as_object_mut().expect("fields").extend(given.as_object().expect("fields").clone());
            shell_status(&shell(fields)).as_str()
        };
        let pending = json!({ "id": "r", "kind": "command", "createdAt": "2026-01-01T00:00:00.000Z" });
        assert_eq!(with(json!({ "pendingRuntimeRequest": pending, "status": "running" })), "approval");
        assert_eq!(with(json!({ "activeRunId": "r", "status": "running" })), "working");
        assert_eq!(with(json!({ "status": "failed" })), "failed");
        assert_eq!(with(json!({ "status": "completed" })), "completed");
        assert_eq!(with(json!({ "status": "idle" })), "idle");
    }
}
