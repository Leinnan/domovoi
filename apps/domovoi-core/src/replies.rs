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
    if previous == Some(ShellStatus::Working) && crate::sidebar::is_live_thread(shell) {
        let beat = match status {
            ShellStatus::Completed | ShellStatus::Idle => "finished",
            ShellStatus::Failed => "failed",
            _ => "",
        };
        if !beat.is_empty() {
            model.companion_beat = (beat.into(), model.companion_beat.1 + 1);
        }
    }
    let watching = model.focused && model.selected_thread.as_deref() == Some(id.as_str());
    let body = notice(previous, status)?;
    // A turn that ended on a plan waits for the plan to be read and carried out.
    let body = if body == "Finished" && shell.has_actionable_proposed_plan { "Plan ready" } else { body };
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
            for mut project in snapshot.projects {
                crate::project_icons::shell_project(&mut model.project_icons, &mut project);
                upsert_project(&mut model.projects, project);
            }
        }
        ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: None } => {
            model.from_cache = false;
            model.cache_dirty = true;
            model.projects = snapshot.projects;
            for project in &mut model.projects {
                crate::project_icons::shell_project(&mut model.project_icons, project);
            }
            crate::project_icons::prune(model);
            model.threads = snapshot.threads;
            // Where each thread stands now, so only a change from here notifies.
            model.notified.clear();
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
        ShellStreamItem::ProjectUpdated { mut project, .. } => {
            model.cache_dirty = true;
            crate::project_icons::shell_project(&mut model.project_icons, &mut project);
            upsert_project(&mut model.projects, *project);
        }
        ShellStreamItem::ProjectRemoved { project_id, .. } => {
            model.projects.retain(|p| p.id != project_id);
            crate::project_icons::prune(model);
        }
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

/// Something for one thread that named another was left out: counted, and
/// noted on the device's log (it means a bug, here or in the server).
fn note_misrouted(model: &mut Model, tid: &str, what: &str) {
    model.misrouted += 1;
    eprintln!("domovoi core: left out {what} on the stream of thread {tid}");
}

/// Starts `tid`'s stream over from a snapshot, nothing held kept.
fn reload_thread(model: &mut Model, tid: &str, why: &str) -> Vec<Cmd> {
    note_misrouted(model, tid, why);
    let stop = update::unsubscribe(model, &Route::Thread(tid.to_owned()));
    model.thread = None;
    model.thread_sequence = 0;
    model.thread_gaps = 0;
    model.changes.frame_is_wide();
    vec![stop, update::subscribe_thread(model, tid)]
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
    // What `tid`'s stream sends goes only into `tid`'s own records. Held
    // records of another thread are not resumed into: the stream starts over
    // from a snapshot of this one.
    if model.thread.as_ref().is_some_and(|held| held.record.id.as_str() != tid) {
        return reload_thread(model, tid, "the thread held was another one");
    }
    match item {
        // A snapshot replaces the thread.
        ThreadStreamItem::Snapshot(snapshot) => {
            let sequence = snapshot.snapshot_sequence;
            let thread = Thread::from_snapshot(*snapshot);
            if thread.record.id.as_str() != tid {
                note_misrouted(model, tid, "a snapshot of another thread");
                return vec![];
            }
            model.changes.frame_is_wide();
            model.cache_dirty = true;
            model.thread_sequence = sequence;
            model.thread = Some(thread);
            model.thread_gaps = 0;
            model.thread_loading = false;
            // The server's copy of a thread being started is in: it shows itself now.
            model.starting_thread.take_if(|l| l.id == tid);
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
            vec![update::request_asset_urls(model)]
        }
        ThreadStreamItem::Event { sequence, event } => {
            if event.thread_id.as_str() != tid {
                note_misrouted(model, tid, "an event of another thread");
                return vec![];
            }
            // What the event asks for besides its record, read before the
            // record goes into the thread.
            let attachments = match &event.body {
                EventBody::MessageUpdated(message) => !message.attachments.is_empty(),
                // A step that looked at an image (asked for once it is done).
                EventBody::TurnItemUpdated(item) => *item.status() != domovoi_model::typed::TurnItemStatus::Running && !crate::tools::image_sources(item, "").is_empty(),
                _ => false,
            };
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
                // The terminal is up: a script it was opened for starts now.
                if let Some(input) = model.panel.terminals[at].pending_input.take() {
                    let request = crate::requests::terminal_write(&thread_id.as_str().into(), terminal_id, &input);
                    return vec![update::call(model, request, Route::Command("Run script".into()))];
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
        Route::ProviderAuth(id) => {
            if let Some(state) = read::<domovoi_model::typed::ProviderAuthState>(model, "a provider's sign-in", v) {
                model.pages.provider_auth.insert(id.clone(), state);
            }
            vec![]
        }
        Route::ProviderInstall(id) => {
            if let Some(state) = read::<domovoi_model::typed::ProviderInstallState>(model, "a provider's installation", v) {
                model.pages.provider_install.insert(id.clone(), state);
            }
            vec![]
        }
        Route::ScheduledTasks => {
            if let Some(list) = read::<ScheduledTaskList>(model, "the scheduled tasks", v) {
                model.pages.scheduled = listed(model, "a scheduled task", list.tasks);
                model.pages.scheduled_time_zone = list.time_zone.flatten().unwrap_or_default();
            }
            // A change to the list (a run started or ended): the runs as they are now.
            let mut cmds = vec![update::call(model, crate::requests::scheduled_tasks_list_runs(None, None), Route::ScheduledRuns(None))];
            if let Some(open) = model.pages.scheduled_runs.as_ref().map(|(id, _)| id.clone()) {
                cmds.push(update::call(model, crate::requests::scheduled_tasks_list_runs(Some(&open), None), Route::ScheduledRuns(Some(open))));
            }
            cmds
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
            // Shown since it was sent: now there is a stream to open.
            vec![update::opened_launched(model, &tid)]
        }
        Route::Send(_) => {
            update::outbox_done(model, &route);
            vec![]
        }
        Route::ProjectIconChanged(id) => {
            crate::project_icons::finish_save(model, &id, Ok(Project::deserialize(value).ok()));
            vec![]
        }
        Route::ProjectIconAsset { .. } | Route::ProjectIconThumbnail { .. } => {
            crate::project_icons::image_reply(model, &route, AssetCreateUrlResult::deserialize(value).ok());
            vec![]
        }
        Route::ProjectIconFiles(request) => {
            crate::project_icons::files_reply(model, &request, ProjectEntriesResult::deserialize(value).map_err(|_| "Could not read project images.".into()));
            vec![]
        }
        Route::Diagram { key, request } => {
            crate::diagrams::reply(model, &key, &request, AssetCreateUrlResult::deserialize(value).ok());
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
            b.parent = crate::tools::parent_dir(&b.path);
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
        Route::PaletteSearch(rev) => {
            let Some(mode) = model.palette.as_ref().filter(|p| p.revision == rev).map(|p| p.mode.clone()) else { return vec![] };
            let results: Vec<crate::view::PaletteItemView> = if mode == "files" {
                read::<ProjectEntriesResult>(model, "the files found", value)
                    .map(|found| found.entries)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|e| e.kind.as_str() == "file")
                    .map(|e| {
                        let (dir, name) = e.path.rsplit_once('/').unwrap_or(("", e.path.as_str()));
                        crate::view::PaletteItemView { id: format!("file:{}", e.path), kind: "file".into(), title: name.to_owned(), subtitle: dir.to_owned(), shortcut: String::new() }
                    })
                    .collect()
            } else {
                read::<domovoi_model::typed::ProjectSearchContentsResult>(model, "the lines found", value)
                    .map(|found| found.matches)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| crate::view::PaletteItemView {
                        id: format!("match:{}:{}", m.path, m.line_number),
                        kind: "match".into(),
                        title: m.line_content.trim().chars().take(120).collect(),
                        subtitle: format!("{}:{}", m.path, m.line_number),
                        shortcut: String::new(),
                    })
                    .collect()
            };
            if let Some(p) = model.palette.as_mut() {
                p.results = results;
                p.searching = false;
                p.highlighted = 0;
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
                    if let Some(expires) = asset.expires_at.as_i64().filter(|expires| *expires > model.now_ms) {
                        model.asset_url_expiries.insert(id.clone(), expires);
                    }
                    model.asset_urls.insert(id, asset.relative_url);
                    vec![render()]
                }
                None => {
                    model.asset_urls.remove(&id);
                    vec![]
                }
            }
        }
        Route::ReadArtifactFile(request) => {
            if model.panel.files.artifact_preview.as_ref().is_some_and(|read| read.request == request && read.current(model)) {
                let file = read::<ProjectReadFileResult>(model, "an HTML artifact", value);
                if let Some(read) = model.panel.files.artifact_preview.as_mut() {
                    let preview = &mut read.preview;
                    preview.loading = false;
                    match file {
                        Some(file) if file.exists != Some(false) => {
                            preview.text = file.contents;
                            preview.truncated = file.truncated;
                        }
                        Some(_) => preview.error = Some("The file does not exist.".into()),
                        None => preview.error = Some("Could not read the HTML artifact.".into()),
                    }
                }
            }
            vec![]
        }
        Route::ReadPanelArtifactFile(request) => {
            let mut extra = vec![];
            if model.panel.artifacts.file_read.as_ref().is_some_and(|read| read.request == request && read.current(model)) {
                let file = read::<ProjectReadFileResult>(model, "an artifact file", value);
                let mut loaded_source: Option<(String, String)> = None;
                if let Some(read) = model.panel.artifacts.file_read.as_mut() {
                    let preview = &mut read.preview;
                    preview.loading = false;
                    match file {
                        Some(file) if file.exists != Some(false) => {
                            let lang = crate::highlight::language_for_path(&preview.path);
                            preview.spans = crate::highlight::highlight(&file.contents, &lang);
                            if !file.truncated && !file.contents.trim().is_empty() {
                                loaded_source = Some((preview.path.clone(), file.contents.clone()));
                            }
                            preview.text = file.contents;
                            preview.truncated = file.truncated;
                        }
                        Some(_) => preview.error = Some("The file does not exist.".into()),
                        None => preview.error = Some("Could not read the artifact file.".into()),
                    }
                }
                if let Some((path, contents)) = loaded_source
                    && let Some(kind) = crate::artifacts::artifact_kind_for_path(&path)
                    && matches!(kind, "react" | "mermaid")
                    && let Some(tid) = model.selected_thread.clone()
                    && let Some(active_id) = model.panel.artifacts.active_id.clone()
                {
                    let dark = model.panel.artifacts.dark;
                    let version = model.panel.artifacts.active_version.unwrap_or(1);
                    let req = if kind == "react" {
                        crate::requests::artifact_react_preview(&contents, dark)
                    } else {
                        crate::requests::mermaid_diagram_svg(&contents, dark)
                    };
                    let url_req_id = update::uuid();
                    model.panel.artifacts.preview_url = Some(crate::model::ArtifactPreviewUrl {
                        request: url_req_id.clone(),
                        thread: tid,
                        artifact_id: active_id,
                        version,
                        dark,
                        url: None,
                        loading: true,
                        error: None,
                    });
                    extra.push(update::call(model, req, Route::ArtifactPreviewUrl(url_req_id)));
                }
            }
            extra
        }
        Route::ArtifactPreviewUrl(request) => {
            if model.panel.artifacts.preview_url.as_ref().is_some_and(|p| p.request == request) {
                let asset = read::<AssetCreateUrlResult>(model, "an artifact preview", value);
                if let Some(p) = model.panel.artifacts.preview_url.as_mut() {
                    p.loading = false;
                    match asset {
                        Some(asset) if !asset.relative_url.contains("missing") => {
                            p.url = Some(asset.relative_url);
                            p.error = None;
                        }
                        _ => {
                            p.url = None;
                        }
                    }
                }
            }
            vec![]
        }
        Route::ReadFile { path, request } => {
            if model.panel.files.preview.as_ref().is_some_and(|p| p.request == request) {
                model.panel.files.preview = Some(match read::<ProjectReadFileResult>(model, "a file", value) {
                    Some(file) => Preview {
                        spans: if file.image_url.is_some() {
                            vec![]
                        } else {
                            crate::highlight::highlight(&file.contents, &crate::highlight::language_for_path(&path))
                        },
                        path,
                        request,
                        text: file.contents,
                        image_url: file.image_url,
                        truncated: file.truncated,
                        error: (file.exists == Some(false)).then(|| "The file does not exist.".into()),
                        loading: false,
                        byte_length: file.byte_length,
                        lfs: file.lfs,
                        fetching: false,
                        fetch_error: None,
                    },
                    None => Preview { path, request, error: Some("This app can't read the server's answer.".into()), ..Default::default() },
                });
            }
            vec![]
        }
        // The object is on the server now (or its state changed): read the file again.
        Route::FetchLfs { path, request } => {
            if model.panel.files.preview.as_ref().is_some_and(|p| p.request == request && p.path == path) {
                return vec![update::update(crate::app::Event::OpenFile { path }, model)];
            }
            vec![]
        }
        Route::Diff { source, of } if source == model.panel.diff.source && of == update::diff_owner(model, &source) => {
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
                model.composer.worktrees = refs.iter().filter(|r| r.is_remote != Some(true)).filter_map(|r| Some((r.name.clone(), r.worktree_path.clone()?))).collect();
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
        Route::ScheduledRuns(task) => {
            let runs = read::<ScheduledRuns>(model, "a scheduled task's runs", value).map(|list| list.runs).unwrap_or_default();
            match task {
                None => model.pages.scheduled_latest_runs = runs,
                Some(task) if model.pages.scheduled_runs.as_ref().is_some_and(|(id, _)| *id == task) => model.pages.scheduled_runs = Some((task, runs)),
                Some(_) => {}
            }
            vec![]
        }
        Route::ThreadPermissionsForgotten => {
            #[derive(serde::Deserialize)]
            struct Forgotten {
                forgotten: usize,
            }
            let n = read::<Forgotten>(model, "the forgotten tools", value).map_or(0, |f| f.forgotten);
            model.toast("success", match n {
                0 => "This thread had no allowed tools to forget.".to_owned(),
                1 => "Forgot 1 allowed tool: its agent asks again from its next start.".to_owned(),
                n => format!("Forgot {n} allowed tools: its agent asks again from its next start."),
            });
            vec![]
        }
        // The card goes once the item says how it ended.
        Route::SecretAnswer { item_id, saved } => {
            model.secrets_answering.remove(&item_id);
            if saved {
                model.toast("success", "Secret saved. The agent can use it now.");
            }
            vec![]
        }
        Route::ScheduledRunNow(task) => vec![update::call(model, crate::requests::scheduled_tasks_list_runs(Some(&task), Some(1)), Route::ScheduledRunStarted(task))],
        Route::ScheduledRunStarted(task) => {
            let run = read::<ScheduledRuns>(model, "a scheduled task's run", value).and_then(|list| list.runs.into_iter().next());
            let title = model.pages.scheduled.iter().find(|t| t.id.as_str() == task).map(|t| t.title.clone()).unwrap_or_default();
            match run {
                Some(run) if run.error.is_some() => model.toast("error", format!("{title}: {}", run.error.unwrap_or_default())),
                Some(crate::model::ScheduledRun { thread_id: Some(thread), .. }) => {
                    model.toast_with_action("success", format!("Started “{title}”."), "Open Thread", &format!("open:{thread}"));
                }
                // A server without run history: no thread to offer.
                _ => model.toast("success", format!("Started “{title}”.")),
            }
            vec![]
        }
        Route::WebhookDeliveries(task) => {
            #[derive(serde::Deserialize)]
            struct Listed {
                deliveries: Vec<crate::model::WebhookDelivery>,
            }
            if let Some(list) = read::<Listed>(model, "the webhook deliveries", value)
                && model.pages.webhook_deliveries.as_ref().is_some_and(|(id, _)| *id == task)
            {
                model.pages.webhook_deliveries = Some((task, list.deliveries));
            }
            vec![]
        }
        Route::Usage => {
            model.pages.usage_loading = false;
            model.pages.usage_error = None;
            let usage = read::<UsageSummary>(model, "the usage summary", value);
            // Rows of a kind this build does not know are not in the totals.
            for _ in usage.iter().flat_map(|usage| usage.buckets.left_out()).filter(|sent| !sent.is_null()) {
                model.left_out("a row of the usage summary", &"an element of a kind this version does not know");
            }
            model.pages.usage = usage;
            vec![]
        }
        Route::UsageLimits => {
            model.pages.usage_limits_refreshing = false;
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
                "Create worktree" | "Remove worktree" => {
                    model.toast("success", if label == "Create worktree" { "Worktree created." } else { "Worktree removed." });
                    let cwd = model.panel.git.cwd.clone();
                    if !cwd.is_empty() {
                        return vec![update::call(model, crate::requests::vcs_list_refs(&cwd), Route::ListRefs(cwd))];
                    }
                }
                "Save file" => model.toast("success", "Saved."),
                "Save plan" => {
                    let path = value.get("relativePath").and_then(Value::as_str).unwrap_or("the workspace").to_owned();
                    model.toast("success", format!("Plan saved to {path}."));
                }
                _ => {}
            }
            vec![]
        }
        _ => vec![],
    }
}

/// What a `SecretRequestError` (which has no message) says, by its reason.
fn secret_failure(cause: &Value) -> Option<&'static str> {
    let error = &cause.as_array()?.first()?["error"];
    if error["_tag"] != "SecretRequestError" {
        return None;
    }
    Some(match error["reason"].as_str()? {
        "not_found" => "the request is gone.",
        "already_answered" => "it was already answered.",
        "agent_stopped" => "the agent stopped waiting for it.",
        "store_failed" | "record_failed" => "the server could not store it. Try again.",
        _ => "the server could not take the answer.",
    })
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
        Route::SecretAnswer { item_id, .. } => {
            model.secrets_answering.remove(item_id);
            model.toast("error", format!("Secret: {}", secret_failure(cause).unwrap_or(&message)));
            return vec![];
        }
        Route::AgentScan => {
            model.sessions.scanning = false;
            model.sessions.error = Some(message);
            return vec![];
        }
        Route::ImportProject { path, .. } | Route::ImportSessions(path) => {
            let name = crate::tools::file_name(path).to_owned();
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
                return vec![update::opened_launched(model, tid)];
            }
            if let Some(out) = out {
                update::give_back(model, &out);
            }
            // Back to a new thread's page, where the text is given back.
            let back = update::launch_failed(model, tid);
            model.toast("error", format!("Start thread: {message}"));
            return vec![back];
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
        Route::Diff { source, of } => {
            if *source != model.panel.diff.source || *of != update::diff_owner(model, source) {
                return vec![];
            }
            model.panel.diff.loading = false;
            model.panel.diff.set_patch("");
            model.panel.diff.error = Some(message);
            return vec![];
        }
        Route::ReadArtifactFile(request) => {
            if model.panel.files.artifact_preview.as_ref().is_some_and(|read| read.request == *request && read.current(model))
                && let Some(read) = model.panel.files.artifact_preview.as_mut()
            {
                read.preview.loading = false;
                read.preview.error = Some(message);
            }
            return vec![];
        }
        Route::ReadPanelArtifactFile(request) => {
            if model.panel.artifacts.file_read.as_ref().is_some_and(|read| read.request == *request && read.current(model))
                && let Some(read) = model.panel.artifacts.file_read.as_mut()
            {
                read.preview.loading = false;
                read.preview.error = Some(message);
            }
            return vec![];
        }
        Route::ArtifactPreviewUrl(request) => {
            if let Some(p) = model.panel.artifacts.preview_url.as_mut().filter(|p| p.request == *request) {
                p.loading = false;
                p.error = Some(message);
            }
            return vec![];
        }
        Route::ReadFile { request, .. } => {
            if let Some(p) = model.panel.files.preview.as_mut().filter(|p| &p.request == request) {
                p.loading = false;
                p.error = Some(message);
            }
            return vec![];
        }
        Route::FetchLfs { request, .. } => {
            if let Some(p) = model.panel.files.preview.as_mut().filter(|p| &p.request == request) {
                p.fetching = false;
                p.fetch_error = Some(message);
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
        Route::ProjectIconChanged(id) => crate::project_icons::finish_save(model, id, Err(())),
        Route::ProjectIconAsset { .. } | Route::ProjectIconThumbnail { .. } => {
            crate::project_icons::image_reply(model, &route, None);
            return vec![];
        }
        Route::ProjectIconFiles(request) => {
            crate::project_icons::files_reply(model, request, Err(message));
            return vec![];
        }
        Route::Diagram { key, request } => {
            crate::diagrams::reply(model, key, request, None);
            return vec![];
        }
        Route::Usage => {
            model.pages.usage_loading = false;
            model.pages.usage_error = Some(message);
            return vec![];
        }
        Route::UsageLimits => model.pages.usage_limits_refreshing = false,
        Route::Command(l) if l == "Pull" => model.panel.git.busy = false,
        Route::PaletteSearch(_) => {
            if let Some(p) = model.palette.as_mut() {
                p.searching = false;
            }
        }
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
        // Runs are Domovoi's own: a server without them answers nothing to show.
        Route::Mention(_) | Route::FileSearch(_) | Route::FindInThread(_) | Route::ListRefs(_) | Route::Archived | Route::ScheduledRuns(_) => return vec![],
        Route::ScheduledRunStarted(task) => {
            let title = model.pages.scheduled.iter().find(|t| t.id.as_str() == task).map(|t| t.title.clone()).unwrap_or_default();
            model.toast("success", format!("Started “{title}”."));
            return vec![];
        }
        Route::Thread(_) => model.thread_loading = false,
        _ => {}
    }
    let label = match &route {
        Route::Command(l) => l.clone(),
        Route::Launch(_) => "Start thread".into(),
        Route::Send(_) => "Send message".into(),
        Route::ProjectCreated => "Add project".into(),
        Route::ProjectIconChanged(_) => "Change project icon".into(),
        Route::Attachments(_) => "Attach images".into(),
        Route::PullRequestAction { .. } => "Pull request".into(),
        Route::PullRequestThread(_) => "Check out the pull request".into(),
        Route::Settings => "Save setting".into(),
        Route::ProviderAuth(_) => "Sign in".into(),
        Route::ProviderInstall(_) => "Install".into(),
        Route::ScheduledRunNow(_) => "Run task".into(),
        Route::ThreadPermissionsForgotten => "Forget allowed tools".into(),
        _ => "Request".into(),
    };
    model.toast("error", format!("{label}: {message}"));
    vec![]
}

/// Applies one reply. Renders nothing: the caller renders once for all the
/// replies a socket frame carried.
/// `scheduledTasks.listRuns`'s answer.
#[derive(serde::Deserialize)]
struct ScheduledRuns {
    runs: Vec<crate::model::ScheduledRun>,
}

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
