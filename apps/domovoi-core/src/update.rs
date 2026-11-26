//! Event handling: user intents become RPCs, orchestration commands and
//! state changes. Server replies are handled in `replies.rs`.
//!
//! A command is the record the server decodes (`domovoi_model::typed::Command`),
//! built here field by field; every other request is made by a function of
//! `requests.rs` from what it is about. Nothing here writes JSON.

use crate::app::{Event, iso_from_millis};
use crate::model::{Attachment, Connection, Launching, Mention, Model, Outgoing, Palette};
use crate::requests::{self, DiffSource, EditedScheduledTask, FindStep, NewScheduledTask, ProjectChange, Request, SettingsPatch};
use crate::rpc::Route;
use crate::socket::{SocketEvent, SocketOperation};
use crate::terminal::{self, Session};
use crate::view::ModelOption;
use crate::Effect;
use crux_core::{Command, render::render};
use crate::settings::{self, Typed};
use domovoi_model::typed::command::{
    ByUser, CheckpointRollback, MessageDispatch, QueueResume, QueuedMessagePromoteToSteer, QueuedRunCancel, QueuedRunEdit, QueuedRunReorder,
    RuntimeRequestRespond, SubagentStop, ThreadArchive, ThreadDelete, ThreadFork, ThreadInteractionModeSet, ThreadMarkUnread, ThreadMergeBack,
    ThreadMetadataUpdate, ThreadModelSelectionSet, ThreadPin, ThreadRuntimeModeSet, ThreadSettle, ThreadSnooze, ThreadStop, ThreadUnarchive,
    ThreadUnpin, ThreadUnsettle, ThreadUnsnooze, ThreadUserInputDismiss, ThreadVisit,
};
use domovoi_model::typed::{
    self, ActivityRunStatus, Actor, ChatAttachment, CommandId, CreationSource, DispatchMode, ForkSourcePoint, GitStackedAction, ImageAttachment,
    InitialMessage, InteractionMode, IntervalSchedule, MessageRole, ModelFavorite, ModelSelection, OptionSelection, OptionValue, ProjectId,
    ProviderInstanceId, ProviderModelPreference, ProviderOptionDescriptor, PullRequestListEntry, PullRequestMergeMethod, RunId, RuntimeMode,
    RuntimeRequestKind, ScheduledTaskSchedule, ServerProviderModel, ServerProviderState, ServerSettings, SourcePlanRef, ThreadId,
    ThreadLaunchInput, ThreadShell, Timestamp, TurnItem, WorkspaceStrategy,
};

pub type Cmd = Command<Effect, Event>;

pub fn send(text: String) -> Cmd {
    Command::notify_shell(SocketOperation::Send { text }).into()
}

pub fn call(model: &mut Model, request: Request, route: Route) -> Cmd {
    if model.connection != Connection::Connected {
        // Something the user did: say it didn't go, rather than nothing.
        if let Route::Command(label) | Route::GitAction(label) = &route
            && label != "Terminal"
        {
            model.toast("error", format!("{label}: not connected to the server. Try again once it's back."));
            return render();
        }
        return Command::done();
    }
    let (_, frame) = model.rpc.request(request.method(), request.payload(), route);
    send(frame)
}

/// Opens a subscription unless one with the same route is open.
pub fn subscribe(model: &mut Model, request: Request, route: Route) -> Cmd {
    if model.rpc.has(&route) {
        return Command::done();
    }
    call(model, request, route)
}

pub fn unsubscribe(model: &mut Model, route: &Route) -> Cmd {
    match model.rpc.id_of(route) {
        Some(id) if model.connection == Connection::Connected => send(model.rpc.interrupt(&id)),
        Some(id) => {
            model.rpc.interrupt(&id);
            Command::done()
        }
        None => Command::done(),
    }
}

/// Asks for the URLs of images in the open thread.
pub fn request_asset_urls(model: &mut Model) -> Cmd {
    if model.connection != Connection::Connected {
        return Command::done();
    }
    let Some(thread) = model.thread.as_ref() else { return Command::done() };
    let shown_messages: std::collections::HashSet<_> = if model.thread_windowed {
        thread.turn_items.iter().filter_map(domovoi_model::typed::TurnItem::message_id).collect()
    } else {
        Default::default()
    };
    let wanted: Vec<ImageAttachment> = thread
        .messages
        .iter()
        .filter(|message| !model.thread_windowed || shown_messages.contains(&message.id))
        .flat_map(|m| &m.attachments)
        .filter_map(|a| match a {
            ChatAttachment::Image(image) if !model.asset_urls.contains_key(&image.id) => Some(image.clone()),
            _ => None,
        })
        .collect();
    // Images the agent's steps looked at or made.
    let thread_id = thread.record.id.as_str().to_owned();
    let cwd = model.thread_cwd(&thread_id).unwrap_or_default();
    let mut shown: Vec<(String, crate::tools::ImageSource, String)> = vec![];
    for item in thread.visible_turn_item_refs() {
        if *item.status() == domovoi_model::typed::TurnItemStatus::Running {
            continue;
        }
        for source in crate::tools::image_sources(item, &cwd) {
            let key = source.key(item.id().as_str());
            if !model.asset_urls.contains_key(&key) && !shown.iter().any(|(k, _, _)| *k == key) {
                shown.push((key, source, item.id().as_str().to_owned()));
            }
        }
    }
    let mut cmds = vec![];
    for image in wanted {
        model.asset_urls.insert(image.id.clone(), String::new());
        cmds.push(call(model, requests::assets_create_url(&image), Route::AssetUrl(image.id)));
    }
    for (key, source, item_id) in shown {
        model.asset_urls.insert(key.clone(), String::new());
        cmds.push(call(model, requests::assets_create_url_for(&source, &thread_id, &item_id), Route::AssetUrl(key)));
    }
    Command::all(cmds)
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_iso(model: &Model) -> String {
    iso_from_millis(model.now_ms)
}

/// The id of a command made now.
fn command_id() -> CommandId {
    CommandId::from(uuid())
}

/// Now, as commands carry it.
fn now(model: &Model) -> Timestamp {
    Timestamp::from(now_iso(model))
}

/// An orchestration command. It waits in the outbox until the server
/// answers, and goes again after a dropped connection (the server answers a
/// repeated command id from its receipt, so nothing happens twice).
fn dispatch(model: &mut Model, command: impl Into<typed::Command>, label: &str) -> Cmd {
    let command = command.into();
    // Marking a thread visited isn't worth queueing.
    if label == "Open thread" {
        let request = requests::orchestration_dispatch_command(command);
        return if model.connection == Connection::Connected { call(model, request, Route::Command(label.into())) } else { Command::done() };
    }
    let out = Outgoing {
        route: Route::Send(command.command_id().as_str().to_owned()),
        request: requests::orchestration_dispatch_command(command),
        message_id: None,
        label: label.into(),
        draft_key: String::new(),
        text: String::new(),
        sent_ms: model.now_ms,
    };
    send_outgoing(model, out)
}

/// The thread that asked `request_id`: the open one, when what it holds is
/// its own and has the request. An answer goes to no other: a card tapped as
/// the page moved to another thread is not that thread's to answer.
fn request_thread(model: &Model, request_id: &str) -> Option<String> {
    let tid = model.selected_thread.as_deref()?;
    model.held_thread(tid)?.runtime_requests.get(request_id)?;
    Some(tid.to_owned())
}

/// Answers a secret request of the open thread, once: unlike a command it
/// waits in no outbox (that would keep the value), so with no connection it
/// is not sent and the card stays to answer again.
fn answer_secret(model: &mut Model, item_id: String, answer: requests::SecretAnswer) -> Cmd {
    let Some(tid) = model.selected_thread.clone() else { return Command::done() };
    let pending = model.held_thread(&tid).and_then(|t| t.turn_items.get(item_id.as_str())).is_some_and(|item| {
        matches!(item, TurnItem::SecretRequest(asked) if asked.secret_status == typed::SecretRequestStatus::Pending)
    });
    if !pending || model.secrets_answering.contains(&item_id) {
        return Command::done();
    }
    if model.connection != Connection::Connected {
        model.toast("error", "Secret: not connected to the server. Try again once it's back.");
        return render();
    }
    let saved = matches!(answer, requests::SecretAnswer::Save(_));
    let request = requests::secrets_answer_request(&ThreadId::from(tid), &item_id, answer);
    model.secrets_answering.insert(item_id.clone());
    Command::all([call(model, request, Route::SecretAnswer { item_id, saved }), render()])
}

/// The user's answer to what an agent asked: a decision on an approval, or
/// with `answers` what was chosen or typed for each question, by its id.
fn respond(thread_id: &str, request_id: &str, decision: &str, answers: Option<Vec<(String, Vec<String>)>>) -> RuntimeRequestRespond {
    RuntimeRequestRespond {
        command_id: command_id(),
        thread_id: thread_id.into(),
        request_id: request_id.into(),
        decision: Some(decision.into()),
        feedback: None,
        answers: answers.map(requests::answers),
        attachments_by_question_id: None,
    }
}

/// The socket is gone with every open request: nothing answers them now,
/// so whatever waits on one stops waiting (sends in the outbox go again).
pub fn requests_lost(model: &mut Model) {
    model.invalidate_history_requests();
    model.rpc.reset();
    model.asset_urls.retain(|_, url| !url.is_empty());
    model.socket_lost_flags();
}

/// The socket's URL. `pack` asks for large frames deflated
/// (`domovoi_rpc::packed`).
fn ws_url(server_url: &str, ticket: &str, pack: bool) -> String {
    let base = server_url.trim_end_matches('/');
    let ws = base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let pack = if pack { "&pack=1" } else { "" };
    format!("{ws}/ws?protocol={}{pack}&wsTicket={ticket}", domovoi_model::PROTOCOL_VERSION)
}

/// Opens the RPC socket with a one-time ticket.
pub fn open_socket(model: &mut Model, ticket: &str) -> Cmd {
    model.rpc.reset();
    // A server on this machine is read from memory: packing would cost both
    // ends time and save nothing. Every other is across a network.
    let across_a_network = model.active_environment().is_some_and(crate::model::Environment::is_remote) || crate::environments::loopback_port(&model.server_url).is_none();
    let url = ws_url(&model.server_url, ticket, across_a_network);
    let attempt = model.attempt;
    Command::stream_from_shell(SocketOperation::Open { url }).then_send(move |event| Event::Socket { attempt, event })
}

/// Everything the client keeps live while connected.
fn subscribe_core(model: &mut Model) -> Cmd {
    let mut cmds = vec![
        subscribe(model, requests::subscribe_server_config(), Route::Config),
        subscribe(model, requests::subscribe_server_lifecycle(), Route::Lifecycle),
        subscribe(model, shell_subscription(model), Route::Shell),
        subscribe(model, requests::subscribe_terminal_metadata(), Route::TerminalMetadata),
        call(model, requests::orchestration_get_archived_shell_snapshot(), Route::Archived),
    ];
    if let Some(t) = model.selected_thread.clone() {
        cmds.push(subscribe_thread(model, &t));
    }
    cmds.push(refresh_context(model));
    Command::all(cmds)
}

/// Restarts the shell and open-thread streams from the last item seen, so
/// a list that looks stale catches up without dropping the connection.
fn resync(model: &mut Model) -> Cmd {
    if model.connection != Connection::Connected {
        return crate::environments::reconnect_now(model);
    }
    let mut cmds = vec![unsubscribe(model, &Route::Shell)];
    cmds.push(call(model, shell_subscription(model), Route::Shell));
    if let Some(t) = model.selected_thread.clone() {
        cmds.push(unsubscribe(model, &Route::Thread(t.clone())));
        cmds.push(subscribe_thread(model, &t));
    }
    cmds.push(refresh_context(model));
    Command::all(cmds).and(render())
}

/// The shell stream, resumed after the last item seen when there was one.
fn shell_subscription(model: &Model) -> Request {
    let held = !model.projects.is_empty() || !model.threads.is_empty();
    requests::orchestration_subscribe_shell(model.shell_sequence.filter(|_| held))
}

pub fn subscribe_thread(model: &mut Model, thread_id: &str) -> Cmd {
    let route = Route::Thread(thread_id.to_owned());
    if model.rpc.has(&route) {
        return Command::done();
    }
    // Already showing it (a reconnect, or opened again from the recent ones):
    // only what happened since, with what is shown left on screen meanwhile.
    // The cursor is resumed only into the thread it is the cursor of.
    let showing = model.held_thread(thread_id).is_some() && model.thread_sequence > 0;
    model.thread_loading = !showing;
    if !showing {
        model.thread_windowed = model.supports_thread_windows && !model.thread_force_full && !(model.panel.open && model.panel.tab == "artifacts");
    }
    let after = showing.then_some(model.thread_sequence);
    let request = if model.thread_windowed {
        requests::orchestration_subscribe_thread_window(&thread_id.into(), after)
    } else {
        requests::orchestration_subscribe_thread(&thread_id.into(), after)
    };
    call(model, request, route)
}

pub fn load_earlier(model: &mut Model) -> Cmd {
    if model.history_loading || model.connection != Connection::Connected {
        return Command::done();
    }
    let (Some(thread), Some(before)) = (model.selected_thread.clone(), model.history_before.as_deref()) else {
        return Command::done();
    };
    let request = requests::orchestration_get_thread_history(&thread.as_str().into(), before);
    model.history_loading = true;
    call(model, request, Route::ThreadHistory { thread_id: thread, generation: model.history_generation, find_revision: None }).and(render())
}

/// Temporarily restores the full projection for actions that need rows across the whole transcript.
pub fn load_full_transcript(model: &mut Model) -> Cmd {
    if model.connection != Connection::Connected {
        return Command::done();
    }
    let Some(tid) = model.selected_thread.clone().filter(|_| model.history_before.is_some()) else {
        return Command::done();
    };
    let route = Route::Thread(tid.clone());
    let stop = unsubscribe(model, &route);
    model.invalidate_history_requests();
    model.thread_windowed = false;
    model.thread_force_full = true;
    model.thread_sequence = 0;
    model.history_before = None;
    stop.and(call(model, requests::orchestration_subscribe_thread(&tid.as_str().into(), None), route))
}

pub fn reveal_history_match(model: &mut Model, entry_id: &str) -> Cmd {
    let Some(thread) = model.selected_thread.as_ref().filter(|_| model.history_before.is_some()).cloned() else {
        return Command::done();
    };
    let request = requests::orchestration_get_thread_history_around(&thread.as_str().into(), entry_id);
    let route = Route::ThreadHistory {
        thread_id: thread, generation: model.history_generation, find_revision: Some(model.find.revision),
    };
    call(model, request, route)
}

fn unsubscribe_threads(model: &mut Model, keep: Option<&str>) -> Cmd {
    let open: Vec<Route> = model.rpc_routes().into_iter().filter(|route| match route {
        Route::Thread(thread) | Route::ThreadHistory { thread_id: thread, .. } | Route::ThreadItemDetail { thread_id: thread, .. } => {
            Some(thread.as_str()) != keep
        }
        Route::FindInThread(_) => model.thread_windowed,
        _ => false,
    }).collect();
    Command::all(open.iter().map(|r| unsubscribe(model, r)).collect::<Vec<_>>())
}

impl Model {
    pub fn rpc_routes(&self) -> Vec<Route> {
        self.rpc.routes()
    }
}

/// Live data for the active working directory: git status, composer branches,
/// and whichever panel tab is showing.
pub fn refresh_context(model: &mut Model) -> Cmd {
    let Some(cwd) = model.active_cwd() else { return Command::done() };
    let mut cmds = vec![];
    if model.panel.git.cwd != cwd {
        let old = model.panel.git.cwd.clone();
        if !old.is_empty() {
            cmds.push(unsubscribe(model, &Route::VcsStatus(old)));
        }
        model.panel.git = Default::default();
        model.panel.git.cwd = cwd.clone();
        model.panel.files = Default::default();
        model.panel.files.root = cwd.clone();
    }
    cmds.push(subscribe(model, requests::subscribe_vcs_status(&cwd), Route::VcsStatus(cwd.clone())));
    if model.composer.branches_cwd != cwd {
        model.composer.branches_cwd = cwd.clone();
        model.composer.branches.clear();
        cmds.push(call(model, requests::vcs_list_refs(&cwd), Route::ListRefs(cwd)));
    }
    if model.panel.open {
        cmds.push(load_panel_tab(model));
    }
    Command::all(cmds)
}

pub fn load_panel_tab(model: &mut Model) -> Cmd {
    match model.panel.tab.as_str() {
        "diff" => load_diff(model),
        "files" => {
            if model.panel.files.children.contains_key("") || model.panel.files.root.is_empty() {
                return Command::done();
            }
            list_dir(model, "")
        }
        "terminal" => {
            if model.panel.terminals.is_empty() && model.selected_thread.is_some() {
                return new_terminal(model);
            }
            Command::done()
        }
        "artifacts" => load_full_transcript(model).and(ensure_active_artifact_loaded(model)),
        _ => Command::done(),
    }
}

fn ensure_active_artifact_loaded(model: &mut Model) -> Cmd {
    let Some(tid) = model.selected_thread.clone() else { return Command::done() };
    let collected = crate::present::collect_artifacts_for_model(model);
    let valid = model.panel.artifacts.thread_id.as_deref() == Some(tid.as_str())
        && model.panel.artifacts.active_id.as_deref().is_some_and(|id| {
            collected.iter().any(|a| a.id == id) || id.starts_with("file:")
        });
    if !valid {
        if let Some(first) = collected.first() {
            model.panel.artifacts.thread_id = Some(tid);
            model.panel.artifacts.active_id = Some(first.id.clone());
            model.panel.artifacts.active_version = None;
            if model.panel.artifacts.mode.is_empty() {
                model.panel.artifacts.mode = "preview".into();
            }
        } else {
            return Command::done();
        }
    }
    load_active_artifact(model)
}

fn load_active_artifact(model: &mut Model) -> Cmd {
    let Some(tid) = model.selected_thread.clone() else { return Command::done() };
    let Some(active_id) = model.panel.artifacts.active_id.clone() else { return Command::done() };
    let dark = model.panel.artifacts.dark;
    let connected = model.connection == Connection::Connected;
    let collected = crate::present::collect_artifacts_for_model(model);
    let mut cmds = vec![];

    let (kind, version, source, path) = if let Some(art) = collected.iter().find(|a| a.id == active_id) {
        let Some(ver) = model
            .panel
            .artifacts
            .active_version
            .and_then(|n| art.versions.iter().find(|v| v.version == n))
            .or_else(|| art.latest())
        else {
            return Command::done();
        };
        (art.kind.clone(), ver.version, ver.source.clone(), ver.path.clone().or_else(|| art.path.clone()))
    } else if let Some(rel) = active_id.strip_prefix("file:") {
        let kind = crate::artifacts::artifact_kind_for_path(rel).unwrap_or("html").to_owned();
        (kind, 1, None, Some(rel.to_owned()))
    } else {
        return Command::done();
    };

    if source.is_none()
        && let Some(rel_path) = path.as_deref()
        && let Some(root) = model.active_cwd()
    {
        let request = uuid();
        model.panel.artifacts.file_read = Some(crate::model::ArtifactRead {
            request: request.clone(),
            thread: tid.clone(),
            root: root.clone(),
            preview: crate::model::Preview {
                path: rel_path.to_owned(),
                loading: connected,
                error: (!connected).then(|| "Connect to the server to preview this file.".into()),
                ..Default::default()
            },
        });
        if connected {
            cmds.push(call(model, requests::projects_read_file(&root, rel_path), Route::ReadPanelArtifactFile(request)));
        }
    }

    if connected {
        let needs_url = !model.panel.artifacts.preview_url.as_ref().is_some_and(|u| {
            u.matches(&tid, &active_id, version) && u.dark == dark && (u.loading || u.url.is_some())
        });
        if needs_url {
            let url_req = if kind == "html" && source.is_none()
                && let Some(rel_path) = path.as_deref()
                && let Some(root) = model.active_cwd()
            {
                Some(requests::artifact_bundle(&root, rel_path))
            } else if kind == "react" && let Some(src) = source.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(requests::artifact_react_preview(src, dark))
            } else if kind == "mermaid" && let Some(src) = source.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(requests::mermaid_diagram_svg(src, dark))
            } else {
                None
            };
            if let Some(req) = url_req {
                let request = uuid();
                model.panel.artifacts.preview_url = Some(crate::model::ArtifactPreviewUrl {
                    request: request.clone(),
                    thread: tid,
                    artifact_id: active_id,
                    version,
                    dark,
                    url: None,
                    loading: true,
                    error: None,
                });
                cmds.push(call(model, req, Route::ArtifactPreviewUrl(request)));
            }
        }
    }

    Command::all(cmds)
}

fn list_dir(model: &mut Model, dir: &str) -> Cmd {
    let cwd = model.panel.files.root.clone();
    model.panel.files.loading.insert(dir.to_owned());
    call(model, requests::projects_list_entries(&cwd, dir), Route::ListEntries(dir.to_owned()))
}

/// The diff sources for the current context, first = default.
pub fn diff_sources(model: &Model) -> Vec<(String, String)> {
    let mut out = vec![];
    if let Some(state) = &model.thread {
        let checkpoints = crate::present::checkpoints(state);
        if !checkpoints.is_empty() {
            out.push(("thread".to_owned(), "All changes in this thread".to_owned()));
            for c in checkpoints.iter().rev() {
                out.push((format!("turn:{}", c.turn_count), format!("Turn {}", c.turn_count)));
            }
        }
    }
    out.push(("working".into(), "Uncommitted changes".into()));
    out.push(("branch".into(), "Branch changes".into()));
    out
}

pub fn load_diff(model: &mut Model) -> Cmd {
    let sources = diff_sources(model);
    if !sources.iter().any(|(id, _)| *id == model.panel.diff.source) {
        model.panel.diff.source = sources.first().map(|(id, _)| id.clone()).unwrap_or_default();
    }
    let source = model.panel.diff.source.clone();
    model.panel.diff.loading = true;
    model.panel.diff.error = None;
    let thread = model.selected_thread.clone();
    let cwd = model.active_cwd().unwrap_or_default();
    let route = Route::Diff { of: diff_owner(model, &source), source: source.clone() };
    match (source.as_str(), thread) {
        ("thread", Some(t)) => {
            let to = model.held_thread(&t).map(|s| crate::present::checkpoints(s).last().map(|c| c.turn_count).unwrap_or(0)).unwrap_or(0);
            call(model, requests::orchestration_get_full_thread_diff(&t.into(), to), route)
        }
        (s, Some(t)) if s.starts_with("turn:") => {
            let n: u32 = s[5..].parse().unwrap_or(1);
            call(model, requests::orchestration_get_turn_diff(&t.into(), n), route)
        }
        ("branch", _) => call(model, requests::review_get_diff_preview(&cwd, DiffSource::BranchRange), route),
        _ => call(model, requests::review_get_diff_preview(&cwd, DiffSource::WorkingTree), route),
    }
}

/// Whose a diff of `source` is now: the open thread's (its own and its
/// turns' diffs), or the checkout's the panel shows.
pub fn diff_owner(model: &Model, source: &str) -> String {
    if source == "thread" || source.starts_with("turn:") {
        model.selected_thread.clone().unwrap_or_default()
    } else {
        model.active_cwd().unwrap_or_default()
    }
}

fn new_terminal(model: &mut Model) -> Cmd {
    let Some(thread) = model.selected_thread.clone() else {
        model.toast("info", "Open a thread to use terminals.");
        return render();
    };
    let cwd = model.active_cwd().unwrap_or_default();
    new_terminal_in(model, &thread, &cwd)
}

fn new_terminal_in(model: &mut Model, thread: &str, cwd: &str) -> Cmd {
    model.panel.terminal_counter += 1;
    let n = model.panel.terminal_counter;
    let id = format!("term-{n}");
    let preference = terminal_preference_key(model, thread, &id);
    model.panel.interactive_terminals.insert(preference);
    model.panel.terminals.push(Session::new(&id, &format!("Terminal {n}")));
    model.panel.terminal_owner = Some(thread.to_owned());
    model.panel.active_terminal = Some(id.clone());
    attach_terminal(model, thread, &id, cwd)
}

fn save_terminal_splits(model: &Model) -> Cmd {
    crate::environments::store(crate::store::StoreOperation::Save {
        key: terminal::SPLIT_KEY.into(),
        value: serde_json::to_string(&model.panel.terminal_splits).expect("terminal split preferences serialize"),
    }).and(render())
}

fn resize_terminal_pane(model: &mut Model, id: &str, rows: u32, cols: u32) -> Cmd {
    let Some(thread) = model.panel.terminal_owner.clone() else { return Command::done() };
    let (rows, cols) = (rows.clamp(1, 500) as u16, cols.clamp(1, 1000) as u16);
    let Some(session) = model.panel.terminals.iter_mut().find(|session| session.id == id) else { return Command::done() };
    if session.watching || !session.running() || session.requested_size == Some((rows, cols)) {
        return Command::done();
    }
    session.requested_size = Some((rows, cols));
    let resizing = model.rpc_routes().iter().any(|route| matches!(route,
        Route::TerminalResize { thread_id, terminal_id, .. } if thread_id == &thread && terminal_id == id));
    if resizing {
        return Command::done();
    }
    send_terminal_size(model, thread, id, rows, cols)
}

fn send_terminal_size(model: &mut Model, thread_id: String, id: &str, rows: u16, cols: u16) -> Cmd {
    let request = requests::terminal_resize(&ThreadId::from(thread_id.as_str()), id, cols, rows);
    call(model, request, Route::TerminalResize { thread_id, terminal_id: id.to_owned(), rows, cols })
}

pub fn terminal_resize_finished(model: &mut Model, thread_id: String, id: &str, sent: (u16, u16), ok: bool) -> Cmd {
    if model.panel.terminal_owner.as_deref() != Some(thread_id.as_str()) {
        return Command::done();
    }
    let Some(session) = model.panel.terminals.iter_mut().find(|terminal| terminal.id == id && !terminal.watching) else { return Command::done() };
    let Some(requested) = session.requested_size else { return Command::done() };
    if requested != sent {
        send_terminal_size(model, thread_id, id, requested.0, requested.1)
    } else {
        if !ok { session.requested_size = None; }
        Command::done()
    }
}

fn terminal_preference_key(model: &Model, thread: &str, id: &str) -> (String, String, String) {
    (model.active_env.clone().unwrap_or_default(), thread.to_owned(), id.to_owned())
}

fn attach_terminal(model: &mut Model, thread: &str, id: &str, cwd: &str) -> Cmd {
    let route = Route::Terminal { thread_id: thread.to_owned(), terminal_id: id.to_owned() };
    let watch = !model.panel.interactive_terminals.contains(&terminal_preference_key(model, thread, id));
    let thread = ThreadId::from(thread);
    let request = if watch {
        requests::terminal_observe(&thread, id)
    } else {
        let (rows, cols) = model.panel.terminals.iter().find(|terminal| terminal.id == id)
            .and_then(|terminal| terminal.requested_size)
            .unwrap_or((terminal::DEFAULT_ROWS, terminal::DEFAULT_COLS));
        requests::terminal_attach(&thread, id, cwd, cols, rows)
    };
    subscribe(model, request, route)
}

fn set_terminal_watch(model: &mut Model, id: &str, watching: bool) -> Cmd {
    let Some(thread) = model.panel.terminal_owner.clone() else { return Command::done() };
    let key = terminal_preference_key(model, &thread, id);
    let Some(session) = model.panel.terminals.iter_mut().find(|terminal| terminal.id == id) else { return Command::done() };
    if session.watching == watching || !session.running() {
        return Command::done();
    }
    session.watching = watching;
    session.invalidate_view();
    if watching {
        model.panel.interactive_terminals.remove(&key);
    } else {
        model.panel.interactive_terminals.insert(key);
    }
    let route = Route::Terminal { thread_id: thread.clone(), terminal_id: id.to_owned() };
    let stop = unsubscribe(model, &route);
    let cwd = model.active_cwd().unwrap_or_default();
    stop.and(attach_terminal(model, &thread, id, &cwd)).and(render())
}

/// Detaches the current thread's terminals (they keep running on the server).
fn detach_terminals(model: &mut Model) -> Cmd {
    let routes: Vec<Route> = model.rpc_routes().into_iter().filter(|r| matches!(r, Route::Terminal { .. })).collect();
    model.panel.terminals.clear();
    model.panel.active_terminal = None;
    model.panel.terminal_owner = None;
    model.panel.focus_terminal = None;
    Command::all(routes.iter().map(|r| unsubscribe(model, r)).collect::<Vec<_>>())
}

/// Reattaches the selected thread's server-side terminals from the metadata stream.
pub fn adopt_terminals(model: &mut Model) -> Cmd {
    let Some(thread) = model.selected_thread.clone() else { return Command::done() };
    let cwd = model.active_cwd().unwrap_or_default();
    let mut cmds = vec![];
    let running: Vec<(String, String)> = model.panel.terminal_summaries.iter().filter(|s| s.thread_id == thread).map(|s| (s.terminal_id.clone(), s.label.clone())).collect();
    for (id, label) in running {
        if model.panel.terminals.iter().any(|t| t.id == id) || id.is_empty() {
            continue;
        }
        let label = if label.is_empty() { id.clone() } else { label };
        let mut session = Session::new(&id, &label);
        session.watching = !model.panel.interactive_terminals.contains(&terminal_preference_key(model, &thread, &id));
        model.panel.terminals.push(session);
        if let Some(n) = id.strip_prefix("term-").and_then(|n| n.parse::<u32>().ok()) {
            model.panel.terminal_counter = model.panel.terminal_counter.max(n);
        }
        model.panel.terminal_owner = Some(thread.clone());
        if model.panel.focus_terminal.as_deref() == Some(id.as_str()) {
            model.panel.active_terminal = Some(id.clone());
            model.panel.focus_terminal = None;
        } else {
            model.panel.active_terminal.get_or_insert(id.clone());
        }
        cmds.push(attach_terminal(model, &thread, &id, &cwd));
    }
    Command::all(cmds)
}

/// Every enabled provider's models: each model's record as the server sent
/// it, with what the composer selects it by.
pub fn model_options(model: &Model) -> Vec<(&ServerProviderModel, ModelOption)> {
    model
        .providers
        .iter()
        .filter(|p| p.enabled)
        .flat_map(|p| {
            let instance = p.instance_id.as_str();
            let provider = p.display_name.as_deref().unwrap_or(instance);
            p.models.iter().map(move |m| {
                let opt = ModelOption { instance_id: instance.to_owned(), model: m.slug.clone(), label: format!("{provider} · {}", m.name) };
                (m, opt)
            })
        })
        .collect()
}

/// Why a provider can't take a message, or empty when it can (or its status
/// is not known yet).
pub fn provider_blocked(model: &Model, instance: &str) -> String {
    let Some(p) = model.providers.iter().find(|p| p.instance_id.as_str() == instance) else { return String::new() };
    if !matches!(p.status, ServerProviderState::Error | ServerProviderState::Disabled) && p.enabled {
        return String::new();
    }
    let name = p.display_name.as_deref().filter(|n| !n.is_empty()).unwrap_or(instance);
    p.message.as_deref().filter(|m| !m.trim().is_empty()).map(str::to_owned).unwrap_or_else(|| format!("{name} can't run on the server right now."))
}

fn provider_ready(model: &Model, instance: &str) -> bool {
    model.providers.iter().any(|p| p.instance_id.as_str() == instance && p.status == ServerProviderState::Ready)
}

/// Whether model `m` answers to `slug`: its own, or one of its aliases (a
/// thread saved on `antigravity-default` runs the model that carries it).
pub fn answers_to(m: &ServerProviderModel, slug: &str) -> bool {
    m.slug == slug || m.aliases.iter().flatten().any(|a| a == slug)
}

/// The listed model of `instance` that `slug` names (its own slug first,
/// else an alias), with how the composer selects it: by `slug`, as saved.
pub fn model_record<'a>(model: &'a Model, instance: &str, slug: &str) -> Option<(&'a ServerProviderModel, ModelOption)> {
    let options = model_options(model);
    let at = options.iter().position(|(m, o)| o.instance_id == instance && m.slug == slug).or_else(|| options.iter().position(|(m, o)| o.instance_id == instance && answers_to(m, slug)))?;
    let (m, o) = options.into_iter().nth(at)?;
    Some((m, ModelOption { model: slug.to_owned(), ..o }))
}

/// The sign-in and install streams of the providers that offer them (the
/// providers page shows them).
fn provider_setup_streams(model: &mut Model) -> Command<Effect, Event> {
    let mut cmds = vec![];
    let setups: Vec<(String, bool, bool)> = model
        .providers
        .iter()
        .filter_map(|p| p.setup.as_ref().map(|s| (p.instance_id.as_str().to_owned(), s.can_authenticate, s.can_install)))
        .collect();
    for (id, auth, install) in setups {
        if auth {
            cmds.push(subscribe(model, requests::provider_auth_subscribe(&id), Route::ProviderAuth(id.clone())));
        }
        if install {
            cmds.push(subscribe(model, requests::provider_install_subscribe(&id), Route::ProviderInstall(id.clone())));
        }
    }
    Command::all(cmds)
}

/// The model a new message uses: the thread's, the composer's choice (while
/// it is still listed), the default from settings, or the first ready
/// provider's default model.
pub fn current_model(model: &Model) -> Option<ModelOption> {
    let options = model_options(model);
    let find = |instance: &str, slug: &str| model_record(model, instance, slug).map(|(_, o)| o);
    if let Some(t) = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)) {
        let (i, m) = (t.model_selection.instance_id.as_str(), t.model_selection.model.as_str());
        return find(i, m).or_else(|| Some(ModelOption { instance_id: i.into(), model: m.into(), label: m.into() }));
    }
    // A pick the provider no longer lists (signed out, another account) is no pick.
    if let Some(m) = &model.composer.model
        && let Some(o) = find(&m.instance_id, &m.model)
    {
        return Some(o);
    }
    if let Some(Some(d)) = model.settings.as_ref().map(|settings| &settings.default_model_selection)
        && let Some(o) = d.as_ref().and_then(|d| find(d.instance_id.as_str(), &d.model))
    {
        return Some(o);
    }
    let ready: Vec<&(&ServerProviderModel, ModelOption)> = options.iter().filter(|(_, o)| provider_ready(model, &o.instance_id)).collect();
    ready.iter().find(|(m, _)| m.is_default == Some(true)).or(ready.first()).map(|(_, o)| o.clone())
}

/// The option descriptors of the model `opt` names.
fn descriptors<'a>(model: &'a Model, opt: &ModelOption) -> &'a [ProviderOptionDescriptor] {
    model_record(model, &opt.instance_id, &opt.model).and_then(|(m, _)| m.capabilities.as_ref()?.option_descriptors.as_deref()).unwrap_or_default()
}

/// Whether a descriptor is a reasoning select by its role (Antigravity's
/// `thought_level`), not by one of the effort ids.
fn reasoning_by_role(d: &ProviderOptionDescriptor) -> bool {
    matches!(d, ProviderOptionDescriptor::Select(s) if s.role == Some(domovoi_model::typed::ProviderOptionRole::Reasoning)) && !matches!(d.id(), Some("effort" | "reasoningEffort"))
}

/// The effort option descriptor of a model: `effort` or `reasoningEffort`,
/// else the select whose role is reasoning (a thinking level).
pub fn effort_descriptor<'a>(model: &'a Model, opt: &ModelOption) -> Option<&'a ProviderOptionDescriptor> {
    let all = descriptors(model, opt);
    all.iter().find(|d| matches!(d.id(), Some("effort" | "reasoningEffort"))).or_else(|| all.iter().find(|d| reasoning_by_role(d)))
}

/// Whether the model's effort can be left to the provider: a thinking level
/// the server has not seen for this model (no current value, no default),
/// shown as "Default" and not sent.
pub fn effort_default_allowed(model: &Model, opt: &ModelOption) -> bool {
    effort_descriptor(model, opt).is_some_and(|d| reasoning_by_role(d) && d.current_value().is_none() && !d.options().iter().any(|o| o.is_default == Some(true)))
}

/// The effort a model runs at here: the one picked for it in this thread (or
/// this new thread), the thread's own when the thread runs that model, or the
/// model's default. Only an effort the model offers: one kept from another
/// model (`xhigh` from Codex) would go to a provider that can't take it.
pub fn current_effort(model: &Model, opt: &ModelOption) -> Option<String> {
    let d = effort_descriptor(model, opt)?;
    let offered = |v: &str| d.options().iter().any(|o| o.id == v);
    let defaultable = effort_default_allowed(model, opt);
    if let Some(e) = model.composer.efforts.get(&effort_key(model, opt)) {
        if offered(e) {
            return Some(e.clone());
        }
        // "Default" picked: left to the provider.
        if e.is_empty() && defaultable {
            return None;
        }
    }
    if let Some(t) = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t))
        && t.model_selection.instance_id.as_str() == opt.instance_id
        && t.model_selection.model == opt.model
    {
        let set = t.model_selection.options.iter().flatten().find(|o| Some(o.id.as_str()) == d.id());
        if let Some(OptionValue::Choice(v)) = set.map(|o| &o.value)
            && offered(v)
        {
            return Some(v.clone());
        }
    }
    if defaultable {
        return None;
    }
    let current = d.current_value().filter(|v| reasoning_by_role(d) && offered(v));
    current.map(str::to_owned).or_else(|| d.options().iter().find(|o| o.is_default == Some(true)).or(d.options().first()).map(|o| o.id.clone()))
}

/// Where an effort picked for `opt` is kept: per thread (or new thread in a
/// project), so a choice in one thread doesn't change another's.
fn effort_key(model: &Model, opt: &ModelOption) -> String {
    format!("{}/{}/{}", model.draft_key(), opt.instance_id, opt.model)
}

/// The model's options other than effort (fast mode, context window…).
pub fn trait_descriptors<'a>(model: &'a Model, opt: &ModelOption) -> Vec<&'a ProviderOptionDescriptor> {
    let effort = effort_descriptor(model, opt).and_then(ProviderOptionDescriptor::id);
    descriptors(model, opt)
        .iter()
        .filter(|d| matches!(d, ProviderOptionDescriptor::Select(_) | ProviderOptionDescriptor::Boolean(_)))
        .filter(|d| !matches!(d.id(), Some("effort" | "reasoningEffort")) && d.id() != effort)
        .collect()
}

/// Where a model option picked for `opt` is kept (beside its effort).
fn trait_key(model: &Model, opt: &ModelOption, id: &str) -> String {
    format!("{}#{id}", effort_key(model, opt))
}

/// A model option as picked here, or as the thread runs it (`None`: left
/// to the provider's default).
pub fn chosen_trait(model: &Model, opt: &ModelOption, d: &ProviderOptionDescriptor) -> Option<OptionValue> {
    let id = d.id()?;
    if let Some(v) = model.composer.efforts.get(&trait_key(model, opt, id)) {
        return Some(match d {
            ProviderOptionDescriptor::Boolean(_) => OptionValue::Switch(v == "true"),
            _ => OptionValue::Choice(v.clone()),
        });
    }
    let t = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t))?;
    (t.model_selection.instance_id.as_str() == opt.instance_id && t.model_selection.model == opt.model)
        .then(|| t.model_selection.options.iter().flatten().find(|o| o.id == id).map(|o| o.value.clone()))
        .flatten()
}

/// The model as a command names it, with the effort and options chosen for it.
fn model_selection(model: &Model, opt: &ModelOption) -> ModelSelection {
    let effort = effort_descriptor(model, opt).and_then(ProviderOptionDescriptor::id).zip(current_effort(model, opt));
    let mut options: Vec<OptionSelection> = effort.map(|(id, effort)| OptionSelection { id: id.to_owned(), value: OptionValue::Choice(effort) }).into_iter().collect();
    for d in trait_descriptors(model, opt) {
        if let (Some(id), Some(value)) = (d.id(), chosen_trait(model, opt, d)) {
            options.push(OptionSelection { id: id.to_owned(), value });
        }
    }
    ModelSelection { instance_id: opt.instance_id.as_str().into(), model: opt.model.clone(), options: (!options.is_empty()).then_some(options) }
}

/// The run a message can join now: the thread's running turn, when its
/// provider takes messages mid-turn (Claude, Codex, Pi).
pub fn steerable_run(model: &Model, shell: &ThreadShell) -> Option<String> {
    let run = shell.active_run_id.as_ref()?;
    if shell.activity_run_status != Some(Some(ActivityRunStatus::Running)) || shell.pending_runtime_request.is_some() {
        return None;
    }
    let instance = shell.provider_instance_id.as_str();
    let driver = model.providers.iter().find(|p| p.instance_id.as_str() == instance).map_or(instance, |p| p.driver.as_str());
    domovoi_model::ProviderDriver::parse(driver).is_some_and(domovoi_model::ProviderDriver::can_steer).then(|| run.as_str().to_owned())
}

pub fn composer_runtime_mode(model: &Model) -> String {
    if let Some(t) = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)) {
        return t.runtime_mode.as_str().to_owned();
    }
    if !model.composer.runtime_mode.is_empty() {
        return model.composer.runtime_mode.clone();
    }
    model.settings.as_ref().and_then(|settings| settings.default_runtime_mode.as_ref()).map_or("full-access", |mode| mode.as_str()).to_owned()
}

pub fn composer_interaction_mode(model: &Model) -> String {
    if let Some(t) = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)) {
        return t.interaction_mode.as_str().to_owned();
    }
    if model.composer.interaction_mode.is_empty() { "default".into() } else { model.composer.interaction_mode.clone() }
}

/// The `@query` being typed at the end of the draft, if any.
pub fn mention_query(draft: &str) -> Option<String> {
    let start = draft.rfind('@')?;
    let q = &draft[start + 1..];
    let boundary = start == 0 || draft[..start].ends_with(char::is_whitespace);
    (boundary && !q.contains(char::is_whitespace) && q.len() <= 120).then(|| q.to_owned())
}

/// A draft opens a slash command menu: `/` and a word being typed.
fn slash_typed(draft: &str) -> bool {
    draft.strip_prefix('/').is_some_and(|q| !q.contains(char::is_whitespace))
}

/// The `$word` being typed at the end of the draft (a skill), as where its
/// `$` is and the word: at the start or after a space, and not money (`$5`).
pub fn skill_query(draft: &str) -> Option<(usize, &str)> {
    let start = draft.rfind('$')?;
    let q = &draft[start + 1..];
    let boundary = start == 0 || draft[..start].ends_with(char::is_whitespace);
    let money = q.chars().next().is_some_and(|c| c.is_ascii_digit());
    (boundary && !money && !q.contains(char::is_whitespace) && q.len() <= 80).then_some((start, q))
}

/// The app's own slash commands (`/model`, `/clear`, `/plan` and
/// `/fork`): what they take after their name, and what they do.
pub const BUILTIN_COMMANDS: [(&str, &str, &str); 4] = [
    ("model", "[model]", "Switch the model (or pick one)"),
    ("plan", "", "Plan first: turn plan mode on or off"),
    ("fork", "", "Fork this thread into a new one"),
    ("clear", "[message]", "Start a fresh session; earlier messages stay visible"),
];

/// A draft that is one of the app's own commands: its name and what follows.
fn builtin_in(draft: &str) -> Option<(&str, &str)> {
    let rest = draft.trim().strip_prefix('/')?;
    let (name, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    BUILTIN_COMMANDS.iter().any(|(n, _, _)| *n == name).then(|| (name, arg.trim()))
}

/// Runs one of the app's own commands (`BUILTIN_COMMANDS`).
fn run_builtin(model: &mut Model, name: &str, arg: &str) -> Cmd {
    let clear_draft = |model: &mut Model| {
        replace_draft(model, String::new());
        model.changes.composer();
    };
    match name {
        "model" if arg.is_empty() => {
            clear_draft(model);
            model.composer.model_picker_requested += 1;
            render()
        }
        "model" => {
            let found = model_options(model)
                .into_iter()
                .filter_map(|(m, opt)| {
                    let provider = model.providers.iter().find(|p| p.instance_id.as_str() == opt.instance_id).and_then(|p| p.display_name.clone()).unwrap_or_default();
                    crate::present::model_match_score(arg, &m.name, &m.slug, &provider).map(|score| (score, opt))
                })
                .min_by_key(|(score, _)| *score)
                .map(|(_, opt)| opt);
            let Some(opt) = found else {
                model.toast("error", format!("No model matches “{arg}”."));
                return render();
            };
            clear_draft(model);
            update(Event::SelectModel { instance_id: opt.instance_id, model: opt.model }, model)
        }
        "plan" => {
            clear_draft(model);
            let mode = if composer_interaction_mode(model) == "plan" { "default" } else { "plan" };
            update(Event::SetInteractionMode { mode: mode.into() }, model)
        }
        "fork" => {
            let Some(tid) = model.selected_thread.clone() else {
                model.toast("error", "Open a thread to fork it.");
                return render();
            };
            clear_draft(model);
            update(Event::ForkThread { thread_id: tid }, model)
        }
        "clear" => {
            let Some(tid) = model.selected_thread.clone() else {
                model.toast("error", "A new thread starts fresh already.");
                return render();
            };
            let text = if arg.is_empty() { "Let's start fresh." } else { arg };
            clear_draft(model);
            let fresh = MessageDispatch {
                fresh_context: Some(true),
                model_selection: current_model(model).map(|opt| model_selection(model, &opt)),
                ..MessageDispatch::new(command_id(), tid.as_str(), uuid(), text, Actor::User, CreationSource::Web)
            };
            dispatch(model, fresh, "Start fresh").and(render())
        }
        _ => Command::done(),
    }
}

fn set_draft(model: &mut Model, text: String) -> Cmd {
    // Only the composer shows a draft.
    model.changes.composer();
    model.drafts_dirty = true;
    let key = model.draft_key();
    // What the composer derives from the draft: whether it can be sent, and the slash menu.
    let before = model.composer.drafts.get(&key).map(String::as_str).unwrap_or_default();
    let derived_moved = before.trim().is_empty() != text.trim().is_empty()
        || slash_typed(before)
        || slash_typed(&text)
        || skill_query(before).is_some()
        || skill_query(&text).is_some();
    model.composer.drafts.insert(key, text.clone());
    let at = text.rfind('@');
    if model.composer.mention_dismissed_at.is_some() && model.composer.mention_dismissed_at != at {
        model.composer.mention_dismissed_at = None;
    }
    let dismissed = at.is_some() && model.composer.mention_dismissed_at == at;
    match mention_query(&text) {
        Some(q) if model.active_cwd().is_some() && !dismissed => {
            let unchanged = model.composer.mention.as_ref().is_some_and(|m| m.query == q);
            if unchanged {
                return render();
            }
            let rev = model.composer.mention.as_ref().map_or(0, |m| m.revision) + 1;
            model.composer.mention = Some(Mention { query: q.clone(), revision: rev, results: vec![], highlighted: 0 });
            let cwd = model.active_cwd().unwrap_or_default();
            call(model, requests::projects_search_entries(&cwd, &q, 8), Route::Mention(rev)).and(render())
        }
        _ => {
            let had = model.composer.mention.take().is_some();
            if had || derived_moved { render() } else { Command::done() }
        }
    }
}

fn replace_draft(model: &mut Model, text: String) {
    let key = model.draft_key();
    model.composer.drafts.insert(key, text);
    model.composer.draft_revision += 1;
}

/// Sends the draft: persists attachments first, then dispatches or launches.
fn send_message(model: &mut Model) -> Cmd {
    let text = model.draft().trim().to_owned();
    model.composer.recall = None;
    // `/model x`, `/plan`, `/fork`, `/clear …` are the app's own, not the agent's.
    if model.composer.attachments.is_empty()
        && let Some((name, arg)) = builtin_in(&text)
    {
        let (name, arg) = (name.to_owned(), arg.to_owned());
        return run_builtin(model, &name, &arg);
    }
    if (text.is_empty() && model.composer.attachments.is_empty()) || model.composer.sending {
        return Command::done();
    }
    let thread_id = model.selected_thread.clone().unwrap_or_else(uuid);
    let message_id = uuid();
    let new_thread = model.selected_thread.is_none();
    if new_thread && model.selected_project.is_none() {
        model.toast("error", "Pick a project first.");
        return render();
    }
    if new_thread && current_model(model).is_none() {
        model.toast("error", "No provider is ready. Check Settings → Providers.");
        return render();
    }
    if let Some(blocked) = current_model(model).map(|m| provider_blocked(model, &m.instance_id)).filter(|b| !b.is_empty()) {
        model.toast("error", blocked);
        return render();
    }
    model.composer.sending = true;
    let pending: Vec<&Attachment> = model.composer.attachments.iter().filter(|a| a.id.is_none()).collect();
    if !pending.is_empty() {
        let request = requests::assets_persist_chat_attachments(&thread_id.as_str().into(), &message_id.as_str().into(), &pending);
        for a in model.composer.attachments.iter_mut().filter(|a| a.id.is_none()) {
            a.uploading = true;
        }
        model.composer.pending_send = Some(text);
        let route = Route::Attachments(SendTarget::now(model).route(&thread_id, &message_id, new_thread));
        return call(model, request, route).and(render());
    }
    let target = SendTarget::now(model);
    finish_send(model, &thread_id, &message_id, new_thread, text, vec![], target)
}

/// Where a send goes, fixed when it is asked for: an upload that finishes
/// later still sends there, whatever is selected by then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendTarget {
    pub draft_key: String,
    pub project: Option<String>,
    pub model: Option<ModelOption>,
}

impl SendTarget {
    pub fn now(model: &Model) -> Self {
        Self { draft_key: model.draft_key(), project: model.selected_project.clone(), model: current_model(model) }
    }

    /// The upload's route: thread, message, whether it starts the thread,
    /// then this target.
    fn route(&self, thread_id: &str, message_id: &str, new_thread: bool) -> Vec<String> {
        let (instance, slug, label) = self.model.as_ref().map_or_else(Default::default, |m| (m.instance_id.clone(), m.model.clone(), m.label.clone()));
        vec![thread_id.into(), message_id.into(), new_thread.to_string(), self.draft_key.clone(), self.project.clone().unwrap_or_default(), instance, slug, label]
    }

    /// The target an upload's route carries (`None` for a route without one).
    pub fn from_route(ctx: &[String]) -> Option<Self> {
        let [_, _, _, draft_key, project, instance, slug, label] = ctx else { return None };
        Some(Self {
            draft_key: draft_key.clone(),
            project: (!project.is_empty()).then(|| project.clone()),
            model: (!instance.is_empty()).then(|| ModelOption { instance_id: instance.clone(), model: slug.clone(), label: label.clone() }),
        })
    }
}

pub fn finish_send(model: &mut Model, thread_id: &str, message_id: &str, new_thread: bool, text: String, attachments: Vec<ChatAttachment>, target: SendTarget) -> Cmd {
    let key = target.draft_key.clone();
    model.composer.drafts.remove(&key);
    model.composer.draft_revision += 1;
    model.composer.attachments.clear();
    model.composer.mention = None;
    model.composer.pending_send = None;
    let steer = std::mem::take(&mut model.composer.steer_next);
    if !new_thread {
        model.composer.sending = false;
        let command_id = uuid();
        // Into the turn under way, or after it (the server queues a steer the
        // turn can't take any more).
        let dispatch_mode = match model.thread_shell(thread_id).filter(|_| steer).and_then(|t| steerable_run(model, t)) {
            Some(run) => DispatchMode::SteerActive { target_run_id: run.into() },
            None => DispatchMode::QueueAfterActive,
        };
        let command = MessageDispatch {
            attachments,
            model_selection: target.model.as_ref().map(|opt| model_selection(model, opt)),
            dispatch_mode,
            ..MessageDispatch::new(command_id.as_str(), thread_id, message_id, text.as_str(), Actor::User, CreationSource::Web)
        };
        let request = requests::orchestration_dispatch_command(command.into());
        let out = Outgoing { request, message_id: Some(message_id.to_owned()), route: Route::Send(command_id), label: "Send message".into(), draft_key: key, text, sent_ms: model.now_ms };
        return send_outgoing(model, out).and(render());
    }
    let project = target.project.clone().unwrap_or_default();
    // Checked before sending, but an upload can outlast the providers that
    // were ready then.
    let Some(opt) = target.model.clone().filter(|o| model_options(model).iter().any(|(_, m)| m.instance_id == o.instance_id && m.model == o.model)) else {
        model.composer.sending = false;
        model.toast("error", "No provider is ready. Check Settings → Providers.");
        return render();
    };
    let title: String = text.lines().next().unwrap_or("New thread").chars().take(60).collect();
    let workspace_strategy = if let Some((worktree_path, branch, _)) = model.composer.existing_worktree.take() {
        WorkspaceStrategy::ExistingWorktree { worktree_path, branch: Some(branch) }
    } else if model.composer.workspace_mode == "worktree" {
        WorkspaceStrategy::Worktree { base_ref: model.composer.base_branch.clone().unwrap_or_else(|| "HEAD".into()), branch: None, start_from_origin: None }
    } else {
        WorkspaceStrategy::Root { branch: None }
    };
    let launch = ThreadLaunchInput {
        command_id: command_id(),
        creation_source: Some(CreationSource::Web),
        thread_id: Some(thread_id.into()),
        reuse_existing_thread: None,
        project_id: project.clone().into(),
        title: if title.is_empty() { "New thread".to_owned() } else { title },
        generate_title: Some(true),
        model_selection: model_selection(model, &opt),
        runtime_mode: composer_runtime_mode(model).as_str().into(),
        interaction_mode: composer_interaction_mode(model).as_str().into(),
        workspace_strategy,
        initial_message: Some(InitialMessage { message_id: Some(message_id.into()), text: text.clone(), context: None, attachments }),
        created_by: None,
        delegated_from: None,
    };
    let request = requests::orchestration_launch_thread(launch);
    let shown_title = launch_title(&text);
    let out = Outgoing { request, message_id: None, route: Route::Launch(thread_id.to_owned()), label: "Start thread".into(), draft_key: key, text: text.clone(), sent_ms: model.now_ms };
    let sent = send_outgoing(model, out);
    // Shown at once, as the server will have it: the thread open, listed,
    // with its first message on its way.
    model.starting_thread = Some(Launching { id: thread_id.to_owned(), project_id: project.clone(), title: shown_title, message_id: message_id.to_owned(), text });
    open_launching(model, thread_id);
    sent.and(render())
}

/// What the sidebar and the page call a thread being started.
fn launch_title(text: &str) -> String {
    let title: String = text.lines().next().unwrap_or_default().chars().take(60).collect();
    if title.trim().is_empty() { "New thread".to_owned() } else { title }
}

/// Opens the thread being started, before the server has it: nothing to
/// subscribe to yet (`opened_launched` does once it is made).
fn open_launching(model: &mut Model, thread_id: &str) {
    model.route = "thread".into();
    model.open_thread(Some(thread_id.to_owned()));
    // Nothing of it to load yet: it shows the message on its way.
    model.thread = None;
    model.thread_sequence = 0;
    model.thread_loading = false;
}

/// The server made the thread being started: its stream, and the visit an
/// opened thread gets, if it is still the one open.
pub fn opened_launched(model: &mut Model, thread_id: &str) -> Cmd {
    if model.selected_thread.as_deref() != Some(thread_id) || model.route != "thread" {
        return Command::done();
    }
    if model.rpc.has(&Route::Thread(thread_id.to_owned())) {
        return render();
    }
    let visit = ThreadVisit { command_id: command_id(), thread_id: thread_id.into(), visited_at: now(model), opened: Some(true), search_query: None };
    let cmds = vec![subscribe_thread(model, thread_id), dispatch(model, visit, "Open thread"), refresh_context(model), adopt_terminals(model)];
    Command::all(cmds).and(render())
}

/// The thread being started was not: it leaves the list, and its page goes
/// back to a new thread's, where the text is given back.
pub fn launch_failed(model: &mut Model, thread_id: &str) -> Cmd {
    let Some(launching) = model.starting_thread.take_if(|l| l.id == thread_id) else { return Command::done() };
    if model.selected_thread.as_deref() == Some(thread_id) {
        return start_new_thread(model, Some(launching.project_id));
    }
    Command::done()
}

/// A server that doesn't answer a send within this long (while connected)
/// gets the text back into the composer.
pub const SEND_TIMEOUT_MS: i64 = 90_000;

fn send_outgoing(model: &mut Model, out: Outgoing) -> Cmd {
    let cmd = call(model, out.request.clone(), out.route.clone());
    model.composer.outbox.push(out);
    cmd
}

/// Sends what the server never answered again, with the same command ids.
fn resend_outbox(model: &mut Model) -> Cmd {
    let now = model.now_ms;
    let mut cmds = Vec::new();
    let items: Vec<(Request, Route)> = model.composer.outbox.iter_mut().map(|o| {
        o.sent_ms = now;
        (o.request.clone(), o.route.clone())
    }).collect();
    for (request, route) in items {
        cmds.push(call(model, request, route));
    }
    Command::all(cmds)
}

/// Takes a send off the outbox once the server answered it.
pub fn outbox_done(model: &mut Model, route: &Route) -> Option<Outgoing> {
    let i = model.composer.outbox.iter().position(|o| &o.route == route)?;
    let out = model.composer.outbox.remove(i);
    if matches!(route, Route::Launch(_)) {
        model.composer.sending = false;
    }
    Some(out)
}

/// A send that didn't arrive: its text goes back where it was typed, unless
/// something new was typed there since.
pub fn give_back(model: &mut Model, out: &Outgoing) {
    let draft = model.composer.drafts.entry(out.draft_key.clone()).or_default();
    if draft.trim().is_empty() && !out.text.is_empty() {
        *draft = out.text.clone();
        model.composer.draft_revision += 1;
    }
}

/// Sends unanswered for too long while connected: given back.
/// What a thread being started that timed out leaves to do: its page goes back.
fn expire_outbox(model: &mut Model) -> Cmd {
    if model.connection != Connection::Connected {
        return Command::done();
    }
    let now = model.now_ms;
    let (late, keep): (Vec<Outgoing>, Vec<Outgoing>) = std::mem::take(&mut model.composer.outbox).into_iter().partition(|o| now - o.sent_ms >= SEND_TIMEOUT_MS);
    model.composer.outbox = keep;
    if late.is_empty() {
        return Command::done();
    }
    for out in &late {
        if let Some(rpc_id) = model.rpc.id_of(&out.route) {
            model.rpc.forget(&rpc_id);
        }
        if matches!(out.route, Route::Launch(_)) {
            model.composer.sending = false;
        }
        give_back(model, out);
    }
    let back: Vec<Cmd> = late.iter().filter_map(|out| if let Route::Launch(tid) = &out.route { Some(launch_failed(model, tid)) } else { None }).collect();
    if late.iter().any(|o| !o.text.is_empty()) {
        model.toast("error", "The server didn't answer, so your message is back in the composer. Check the thread list before sending it again.");
    } else {
        let labels: Vec<&str> = late.iter().map(|o| o.label.as_str()).collect();
        model.toast("error", format!("The server didn't answer: {} may not have happened.", labels.join(", ")));
    }
    Command::all(back)
}

/// Saves how this device lays out the thread list.
fn save_sidebar_layout(model: &Model) -> Cmd {
    let value = serde_json::to_string(&model.sidebar_layout).expect("plain data serializes");
    crate::environments::store(crate::store::StoreOperation::Save { key: crate::sidebar::KEY_SIDEBAR_LAYOUT.into(), value })
}

pub fn select_thread(model: &mut Model, thread_id: String) -> Cmd {
    if model.selected_thread.as_deref() != Some(thread_id.as_str()) {
        model.panel.files.artifact_preview = None;
        model.panel.files.preview = None;
        model.panel.artifacts.active_id = None;
        model.panel.artifacts.active_version = None;
        model.panel.artifacts.file_read = None;
        model.panel.artifacts.preview_url = None;
    }
    if model.selected_thread.as_deref() == Some(thread_id.as_str()) && model.route == "thread" {
        return render();
    }
    let project = model.thread_shell(&thread_id).map(|t| t.project_id.as_str().to_owned());
    let mut cmds = vec![unsubscribe_threads(model, Some(&thread_id)), detach_terminals(model)];
    model.route = "thread".into();
    model.held_section = crate::sidebar::hold_for(model, &thread_id);
    model.drafts_dirty = true;
    if project.is_some() {
        model.selected_project = project;
    }
    // The thread left is kept; one opened lately is shown at once, as it was,
    // and its stream resumes after what it had.
    model.open_thread(Some(thread_id.clone()));
    model.composer.mention = None;
    model.composer.attachments.clear();
    cmds.push(subscribe_thread(model, &thread_id));
    let visit = ThreadVisit { command_id: command_id(), thread_id: thread_id.as_str().into(), visited_at: now(model), opened: Some(true), search_query: None };
    cmds.push(dispatch(model, visit, "Open thread"));
    cmds.push(refresh_context(model));
    cmds.push(adopt_terminals(model));
    Command::all(cmds).and(render())
}

/// The next folder of an import: its project first when it has none, then
/// its sessions. Done when the queue is empty.
pub fn import_next(model: &mut Model) -> Cmd {
    if model.sessions.queue.is_empty() {
        return Command::done();
    }
    let (path, existing) = model.sessions.queue.remove(0);
    match existing {
        Some(project_id) => import_sessions(model, &path, &project_id),
        None => {
            let title = model
                .sessions
                .candidates
                .iter()
                .find(|c| c.path == path)
                .map(|c| c.title.clone())
                .unwrap_or_else(|| crate::tools::file_name(&path).to_owned());
            let project_id = uuid();
            let create = ProjectChange::Create { project: &ProjectId::from(project_id.as_str()), title: &title, workspace_root: &path };
            call(model, requests::projects_mutate(&uuid(), create), Route::ImportProject { path, project_id })
        }
    }
}

pub fn import_sessions(model: &mut Model, path: &str, project_id: &str) -> Cmd {
    call(model, requests::agent_sessions_import(&project_id.into(), path), Route::ImportSessions(path.to_owned()))
}

/// A call about one pull request, which `request` makes of its entry in
/// the list; `done` shows once it worked.
pub(crate) fn reload_pr_review(model: &mut Model, entry: &PullRequestListEntry, key: String) -> Cmd {
    call(model, requests::pull_requests_reviews_get(entry), Route::PrReview(key))
}

fn pr_call(model: &mut Model, key: &str, request: impl FnOnce(&PullRequestListEntry) -> Request, done: &str) -> Cmd {
    let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key) else { return Command::done() };
    let request = request(e);
    model.pages.pr_busy = Some(key.to_owned());
    call(model, request, Route::PullRequestAction { key: key.to_owned(), done: done.to_owned() }).and(render())
}

pub fn start_new_thread(model: &mut Model, project_id: Option<String>) -> Cmd {
    model.composer.existing_worktree = None;
    let mut cmds = vec![unsubscribe_threads(model, None), detach_terminals(model)];
    if let Some(p) = project_id {
        model.selected_project = Some(p);
    }
    if model.selected_project.is_none() {
        model.selected_project = model.projects.first().map(|p| p.id.as_str().to_owned());
    }
    model.route = "new-thread".into();
    model.open_thread(None);
    model.composer.attachments.clear();
    model.composer.draft_revision += 1;
    cmds.push(refresh_context(model));
    Command::all(cmds).and(render())
}

/// Shown at once, confirmed by the server's answer, undone if it refuses.
fn setting_patch(model: &mut Model, key: &str, typed: Typed) -> Cmd {
    // As the settings record holds it, where it can.
    let patch = match settings::patch(key, &typed) {
        Some(settings) => requests::server_update_settings(SettingsPatch::Settings(&settings)),
        None => requests::server_update_settings(SettingsPatch::AsTyped { key, value: &typed }),
    };
    settings_change(model, patch, |model| settings::show(model, key, typed))
}

/// Sends a settings patch after making its change to the local copy with
/// `apply` (shown at once, confirmed by the server's answer, undone if it
/// refuses).
fn settings_change(model: &mut Model, patch: Request, apply: impl FnOnce(&mut Model)) -> Cmd {
    if model.connection != Connection::Connected {
        model.toast("error", "Not connected to the server, so the setting wasn't changed.");
        return render();
    }
    if model.settings_before.is_none() {
        model.settings_before = Some(model.settings.clone());
    }
    apply(model);
    call(model, patch, Route::Settings).and(render())
}

/// The favorite models, `(instance id, slug)`, in the order they were added.
pub fn model_favorites(settings: Option<&ServerSettings>) -> Vec<(String, String)> {
    let favorites = settings.and_then(|settings| settings.favorites.as_ref()?.as_ref());
    favorites.into_iter().flatten().map(|f| (f.provider.as_str().to_owned(), f.model.clone())).collect()
}

/// What the user set for a provider's models in the picker.
pub fn model_preference<'a>(settings: Option<&'a ServerSettings>, instance: &str) -> Option<&'a ProviderModelPreference> {
    settings?.provider_model_preferences.as_ref()?.as_ref()?.get(instance)
}

/// The slugs of a provider's models hidden from the picker.
pub fn hidden_models(settings: Option<&ServerSettings>, instance: &str) -> Vec<String> {
    model_preference(settings, instance).and_then(|preference| preference.hidden_models.clone()).unwrap_or_default()
}

/// Marks the local copy the way the server's `setModelFavorites` will.
fn mark_favorite(model: &mut Model, instance: &str, slug: &str, favorite: bool) {
    let settings = model.settings.get_or_insert_with(ServerSettings::default);
    let mut list = settings.favorites.take().flatten().unwrap_or_default();
    list.retain(|f| !(f.provider.as_str() == instance && f.model == slug));
    if favorite {
        list.push(ModelFavorite { provider: ProviderInstanceId::from(instance), model: slug.to_owned() });
    }
    settings.favorites = Some(Some(list));
}

/// Marks the local copy the way the server's `setModelsHidden` will.
fn mark_hidden(model: &mut Model, instance: &str, slug: &str, hidden: bool) {
    let settings = model.settings.get_or_insert_with(ServerSettings::default);
    let preferences = settings.provider_model_preferences.take().flatten().unwrap_or_default();
    let preferences = settings.provider_model_preferences.insert(Some(preferences)).get_or_insert_with(Default::default);
    let entry = preferences.entry(ProviderInstanceId::from(instance)).or_insert_with(|| ProviderModelPreference { hidden_models: Some(vec![]), model_order: Some(vec![]) });
    let mut list = entry.hidden_models.take().unwrap_or_default();
    list.retain(|m| m != slug);
    if hidden {
        list.push(slug.to_owned());
    }
    entry.hidden_models = Some(list);
    entry.model_order.get_or_insert_with(Vec::new);
}

fn set_model_favorite(model: &mut Model, instance: String, slug: String, favorite: bool) -> Cmd {
    let unhide = favorite && hidden_models(model.settings.as_ref(), &instance).contains(&slug);
    let provider = ProviderInstanceId::from(instance.as_str());
    let patch = SettingsPatch::Model { provider: &provider, model: &slug, favorite: Some(favorite), hidden: unhide.then_some(false) };
    settings_change(model, requests::server_update_settings(patch), |model| {
        mark_favorite(model, &instance, &slug, favorite);
        if unhide {
            mark_hidden(model, &instance, &slug, false);
        }
    })
}

fn set_model_hidden(model: &mut Model, instance: String, slug: String, hidden: bool) -> Cmd {
    let unfavorite = hidden && model_favorites(model.settings.as_ref()).iter().any(|(i, m)| *i == instance && *m == slug);
    let provider = ProviderInstanceId::from(instance.as_str());
    let patch = SettingsPatch::Model { provider: &provider, model: &slug, favorite: unfavorite.then_some(false), hidden: Some(hidden) };
    settings_change(model, requests::server_update_settings(patch), |model| {
        mark_hidden(model, &instance, &slug, hidden);
        if unfavorite {
            mark_favorite(model, &instance, &slug, false);
        }
    })
}

/// Searches the open workspace for what the palette's files or content mode
/// has typed.
fn palette_search(model: &mut Model) -> Cmd {
    let Some(cwd) = model.active_cwd() else { return Command::done() };
    let Some(p) = model.palette.as_mut() else { return Command::done() };
    let query = p.query.trim().to_owned();
    let short = query.is_empty() || (p.mode == "content" && query.chars().count() < 2);
    if !matches!(p.mode.as_str(), "files" | "content") || short {
        p.results.clear();
        p.searching = false;
        return Command::done();
    }
    p.revision += 1;
    p.searching = true;
    let rev = p.revision;
    let request = if p.mode == "files" { requests::projects_search_entries(&cwd, &query, 30) } else { requests::projects_search_contents(&cwd, &query, 50) };
    call(model, request, Route::PaletteSearch(rev))
}

fn palette_command(model: &mut Model, id: &str) -> Cmd {
    // Changing mode keeps the palette open.
    match id {
        "action:go-to-file" => return update(Event::SetPaletteMode { mode: "files".into() }, model),
        "action:search-project" => return update(Event::SetPaletteMode { mode: "content".into() }, model),
        _ => {}
    }
    model.palette = None;
    if let Some(path) = id.strip_prefix("file:").or_else(|| id.strip_prefix("match:").map(|m| m.rsplit_once(':').map_or(m, |(path, _)| path))) {
        let path = path.to_owned();
        return update(Event::ShowPanel { tab: "files".into() }, model).and(update(Event::OpenFile { path }, model));
    }
    if let Some(t) = id.strip_prefix("thread:") {
        return select_thread(model, t.to_owned());
    }
    if let Some(p) = id.strip_prefix("project:") {
        return start_new_thread(model, Some(p.to_owned()));
    }
    match id {
        "action:new-thread" => start_new_thread(model, None),
        "action:toggle-panel" => update(Event::TogglePanel, model),
        "action:diff" => update(Event::ShowPanel { tab: "diff".into() }, model),
        "action:artifacts" => update(Event::ShowPanel { tab: "artifacts".into() }, model),
        "action:terminal" => update(Event::ShowPanel { tab: "terminal".into() }, model),
        "action:files" => update(Event::ShowPanel { tab: "files".into() }, model),
        "action:git" => update(Event::ShowPanel { tab: "git".into() }, model),
        "action:pull-requests" => update(Event::ShowRoute { route: "pull-requests".into() }, model),
        "action:usage" => update(Event::ShowRoute { route: "usage".into() }, model),
        "action:scratch" => update(Event::OpenScratch, model),
        "action:find" => update(Event::FindInThread { query: String::new() }, model),
        "action:settle" => match model.selected_thread.clone() {
            Some(t) => update(Event::SettleThread { thread_id: t, settled: true }, model),
            None => render(),
        },
        "action:pin" => match model.selected_thread.clone() {
            Some(t) => {
                let pinned = model.thread_shell(&t).is_some_and(|s| matches!(s.pinned_at, Some(Some(_))));
                update(Event::PinThread { thread_id: t, pinned: !pinned }, model)
            }
            None => render(),
        },
        _ => render(),
    }
}

/// `plans/<slug>.md` from a plan's first heading or line.
pub fn plan_file_name(markdown: &str) -> String {
    let first = markdown.lines().map(|l| l.trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("plan");
    let mut slug = String::new();
    for c in first.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    format!("plans/{}.md", if slug.is_empty() { "plan" } else { slug })
}

fn utc_day(ms: i64) -> String {
    iso_from_millis(ms)[..10].to_owned()
}

fn local_day(model: &Model, ms: i64) -> String {
    utc_day(ms.saturating_add(i64::from(model.utc_offset_secs) * 1_000))
}

pub fn update(event: Event, model: &mut Model) -> Cmd {
    match event {
        // ---- connection ----
        Event::Launch { surface, device_label, device_os } => crate::environments::launch(model, surface, device_label, device_os),
        // A link or a tap while saved servers load waits for them.
        Event::ConnectRemote { .. } | Event::OpenLink { .. } | Event::OpenNotification { .. } | Event::RespondFromNotification { .. } | Event::AnswerFromNotification { .. } | Event::RespondToThread { .. } if model.launching => {
            model.deferred.push(event);
            Command::done()
        }
        Event::EnvironmentsLoaded { value } => crate::environments::environments_loaded(model, value),
        Event::ActiveEnvironmentLoaded { value } => crate::environments::active_loaded(model, value),
        Event::DraftsLoaded { environment_id, value } => crate::offline::drafts_loaded(model, environment_id, value),
        Event::CacheLoaded { environment_id, value } => crate::offline::cache_loaded(model, environment_id, value),
        Event::TokenLoaded { environment_id, value } => crate::environments::token_loaded(model, environment_id, value),
        Event::Stored => Command::done(),
        Event::Connect { server_url, pairing_code } => crate::environments::connect_local(model, server_url, pairing_code),
        Event::ConnectRemote { link } => crate::environments::connect_remote(model, link),
        Event::OpenLink { link } => crate::environments::open_link(model, link),
        Event::AnswerLinkRequest { id, accept } => crate::environments::answer_link_request(model, id, accept),
        Event::ConnectLocalServer => crate::environments::connect_local_asked(model),
        Event::LocalAutoLoaded { value } => crate::environments::local_auto_loaded(model, value),
        Event::LocalServer { out } => crate::environments::local_server_output(model, out),
        Event::LocalRequested { attempt, out } => crate::environments::local_requested(model, attempt, out),
        Event::LocalSignedIn { attempt, res } => crate::environments::local_signed_in(model, attempt, res),
        Event::ForgetTokenLoaded { id, label, url, value } => crate::environments::forget_token_loaded(model, id, label, url, value),
        Event::ForgottenSignedOut { label, res } => crate::environments::forgotten_signed_out(model, label, res),
        Event::SelectEnvironment { id } => {
            model.pending_terminal_target = None;
            if model.active_env.as_deref() == Some(id.as_str()) && model.connection == Connection::Connected {
                return Command::done();
            }
            crate::environments::select(model, id, None)
        }
        Event::ForgetEnvironment { id } => crate::environments::forget(model, id),
        Event::SignedOut { id } => crate::environments::signed_out(model, id),
        Event::RenameEnvironment { id, label } => crate::environments::rename(model, id, label),
        Event::SetCompanion { id, look } => crate::companion::save(model, &id, Some(look)),
        Event::ResetCompanion { id } => crate::companion::save(model, &id, None),
        Event::MoveEnvironment { id, delta } => crate::environments::move_environment(model, id, delta),
        Event::TokenIssued { attempt, environment_id, res } => crate::environments::token_issued(model, attempt, environment_id, res),
        Event::FallbackTokenLoaded { attempt, environment_id, value } => crate::environments::fallback_token_loaded(model, attempt, environment_id, value),
        Event::TicketIssued { attempt, res } => {
            if attempt != model.attempt {
                return Command::done();
            }
            crate::environments::ticket_issued(model, res)
        }
        Event::Tunnel { tunnel_gen, out } => {
            if tunnel_gen != model.tunnel_gen {
                return Command::done();
            }
            crate::environments::tunnel_output(model, out)
        }
        // A socket of an abandoned attempt says nothing about this one.
        Event::Socket { attempt, .. } if attempt != model.attempt => Command::done(),
        Event::AppendToDraft { text } => {
            let key = model.draft_key();
            let draft = model.composer.drafts.entry(key).or_default();
            let trimmed = draft.trim_end().to_owned();
            *draft = if trimmed.is_empty() { text } else { format!("{trimmed}\n\n{text}") };
            model.composer.draft_revision += 1;
            model.drafts_dirty = true;
            render()
        }
        Event::EditMessage { text } => {
            let key = model.draft_key();
            model.composer.drafts.insert(key, text);
            model.composer.draft_revision += 1;
            render()
        }
        Event::ReloadThread => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let route = Route::Thread(tid.clone());
            let stop = unsubscribe(model, &route);
            model.thread = None;
            model.thread_sequence = 0;
            model.thread_gaps = 0;
            stop.and(subscribe_thread(model, &tid)).and(render())
        }
        Event::LoadEarlier => load_earlier(model),
        Event::ExpandItem { id } => {
            let detail = model.thread.as_ref().and_then(|thread| {
                let domovoi_model::typed::TurnItem::CommandExecution(command) = thread.turn_items.get(id.as_str())? else { return None };
                (command.output_omitted == Some(true)).then(|| thread.record.id.as_str().to_owned())
            });
            model.expanded.insert(id.clone());
            if let Some(thread_id) = detail {
                let request = requests::orchestration_get_turn_item(&thread_id, &id);
                let route = Route::ThreadItemDetail { thread_id, item_id: id, generation: model.history_generation };
                if !model.rpc.has(&route) {
                    return call(model, request, route).and(render());
                }
            }
            render()
        }
        Event::CancelSend { id } => {
            let route = Route::Send(id);
            // Only one still waiting: one the server may have taken can't be unsent.
            if model.connection == Connection::Connected {
                return Command::done();
            }
            if let Some(out) = outbox_done(model, &route) {
                give_back(model, &out);
            }
            render()
        }
        Event::Reconnect => crate::environments::reconnect_now(model),
        Event::Resync => resync(model),
        Event::RetryTimer { retry_gen } => crate::environments::retry_timer(model, retry_gen),
        Event::NetworkStatus { online } => crate::environments::network_status(model, online),
        Event::Suspend => crate::environments::suspend(model),
        Event::Resume => crate::environments::resume(model),
        Event::NetworkChanged => crate::environments::network_changed(model),
        Event::PushToken { token, environment } => crate::environments::push_token(model, token, environment),
        Event::PushEndpoint { endpoint, p256dh, auth } => crate::environments::push_endpoint(model, endpoint, p256dh, auth),
        Event::ActivityToken { thread_id, token, environment } => crate::environments::activity_token(model, thread_id, token, environment),
        Event::OpenNotification { environment_id, thread_id } => crate::environments::open_notification(model, environment_id, thread_id),
        Event::RespondFromNotification { environment_id, thread_id, request_id, decision } => {
            // Only the server in use: answering another would switch servers unseen.
            if !crate::environments::is_current_server(model, &environment_id) {
                return Command::done();
            }
            let respond = dispatch(model, respond(&thread_id, &request_id, &decision, None), "Respond");
            respond.and(crate::environments::resume(model))
        }
        Event::RespondToThread { environment_id, thread_id, decision } => {
            if !crate::environments::is_current_server(model, &environment_id) {
                return Command::done();
            }
            // The approval waiting in the thread (a question needs the app).
            let pending = model.threads.iter().find(|t| t.id.as_str() == thread_id).and_then(|t| t.pending_runtime_request.as_ref());
            let Some(request_id) = pending.filter(|pending| pending.kind != RuntimeRequestKind::UserInput).map(|pending| pending.id.as_str().to_owned()) else { return Command::done() };
            let respond = dispatch(model, respond(&thread_id, &request_id, &decision, None), "Respond");
            respond.and(crate::environments::resume(model))
        }
        Event::AnswerFromNotification { environment_id, thread_id, request_id, question_id, text } => {
            let text = text.trim().to_owned();
            if !crate::environments::is_current_server(model, &environment_id) || text.is_empty() {
                return Command::done();
            }
            let answer = dispatch(model, respond(&thread_id, &request_id, "accept", Some(vec![(question_id, vec![text])])), "Answer");
            answer.and(crate::environments::resume(model))
        }
        Event::PushRegistered(_) => Command::done(),
        Event::LoadRemoteAccess => crate::environments::load_remote_access(model),
        Event::SetRemoteAccess { enabled } => crate::environments::set_remote_access(model, enabled),
        Event::CreatePairingLink => crate::environments::create_pairing_link(model),
        Event::RevokeClient { session_id } => crate::environments::revoke_client(model, session_id),
        Event::RemoteAccessLoaded(res) => crate::environments::remote_access_loaded(model, res),
        Event::PairingLinkCreated(res) => crate::environments::pairing_link_created(model, res),
        Event::ClientRevoked(res) => crate::environments::client_revoked(model, res),
        Event::Socket { event: SocketEvent::Opened, .. } => {
            model.connection = Connection::Connected;
            model.error = None;
            model.in_flight = false;
            model.retry_in = 0;
            // Attempts reset once the server answers (its welcome): one that
            // accepts the socket and drops it at once keeps backing off.
            // Terminals reattach through the metadata stream.
            model.panel.terminals.clear();
            model.panel.active_terminal = None;
            model.panel.terminal_details.clear();
            model.panel.terminal_details_available = false;
            model.panel.git.cwd.clear();
            model.composer.branches_cwd.clear();
            model.refusals = 0;
            let core = subscribe_core(model);
            core.and(resend_outbox(model)).and(render())
        }
        Event::Socket { event: SocketEvent::Message { text }, .. } => {
            // One render for the whole frame, however many replies it carried.
            let replies = model.rpc.receive(&text);
            let render_after = !replies.is_empty();
            let cmds = Command::all(replies.into_iter().map(|r| crate::replies::handle(model, r)).collect::<Vec<_>>());
            if render_after { cmds.and(render()) } else { cmds }
        }
        Event::Socket { event: SocketEvent::Closed { reason }, .. } => {
            let was = model.connection;
            // Uploads start over: the message goes back with its images.
            if let Some(text) = model.composer.pending_send.take() {
                model.composer.sending = false;
                for a in model.composer.attachments.iter_mut() {
                    a.uploading = false;
                }
                let key = model.draft_key();
                let draft = model.composer.drafts.entry(key).or_default();
                if draft.trim().is_empty() {
                    *draft = text;
                    model.composer.draft_revision += 1;
                }
            }
            requests_lost(model);
            model.in_flight = false;
            // Later replies of this attempt are of no use.
            model.attempt += 1;
            if model.manual_disconnect || model.suspended {
                model.connection = Connection::Disconnected;
            } else if was == Connection::Connected || was == Connection::Reconnecting || model.token.is_some() {
                model.connection = Connection::Reconnecting;
                crate::environments::schedule_retry(model);
            } else {
                model.connection = Connection::Failed;
                if !reason.is_empty() {
                    model.error = Some(reason);
                }
            }
            render()
        }
        Event::Disconnect => crate::environments::disconnect(model),
        Event::Tick { now_ms } => {
            model.now_ms = now_ms;
            let pending = model.composer.outbox.len();
            let mut save = if model.drafts_dirty { crate::offline::save_drafts(model) } else { Command::done() };
            if model.cache_dirty && now_ms - model.cache_saved_ms >= crate::offline::CACHE_EVERY_MS {
                save = save.and(crate::offline::save_cache(model));
            }
            save = save.and(expire_outbox(model));
            let mut changed = model.composer.outbox.len() != pending;
            let mut assets_expired = false;
            model.asset_url_expiries.retain(|key, expires| {
                if *expires > now_ms {
                    return true;
                }
                model.asset_urls.remove(key);
                assets_expired = true;
                false
            });
            if assets_expired {
                save = save.and(request_asset_urls(model));
                changed = true;
            }
            // A pairing link past its expiry is of no use: stop showing it.
            if model.pages.remote_link.as_ref().is_some_and(|link| crate::app::millis_from_iso(link.expires_at.as_str()) <= now_ms) {
                model.pages.remote_link = None;
                changed = true;
            }
            // Relative times ("2 min ago") need a render now and then.
            if changed || now_ms - model.tick_rendered_ms >= 20_000 {
                model.tick_rendered_ms = now_ms;
                return save.and(render());
            }
            // The view tells ages from `shown_ms`: a quiet tick shows nothing new.
            model.changes.nothing_shown();
            save
        }
        Event::AppFocus { focused } => {
            model.focused = focused;
            model.changes.nothing_shown();
            Command::done()
        }
        Event::DismissToast => {
            model.toast = model.toast_queue.pop_front();
            render()
        }
        Event::ShowNotice { kind, message } => {
            let kind = if matches!(kind.as_str(), "error" | "info" | "success") { kind } else { "info".to_owned() };
            model.toast(&kind, message);
            render()
        }
        Event::ToastAction => {
            let action = model.toast.as_ref().map(|t| t.action.clone()).unwrap_or_default();
            model.toast = model.toast_queue.pop_front();
            match action.split_once(':') {
                Some(("unsnooze", thread_id)) => {
                    let wake = ThreadUnsnooze { command_id: command_id(), thread_id: thread_id.into(), reason: Some(ByUser::User) };
                    dispatch(model, wake, "Wake thread").and(render())
                }
                Some(("open", thread_id)) => select_thread(model, thread_id.to_owned()),
                Some(("unarchive", thread_id)) => {
                    let c = dispatch(model, ThreadUnarchive { command_id: command_id(), thread_id: thread_id.into() }, "Unarchive thread");
                    let refresh = call(model, requests::orchestration_get_archived_shell_snapshot(), Route::Archived);
                    c.and(refresh).and(render())
                }
                _ => render(),
            }
        }
        Event::Notified => Command::done(),

        // ---- navigation / sidebar ----
        Event::SelectThread { thread_id } => select_thread(model, thread_id),
        Event::NewThread { project_id } => start_new_thread(model, project_id),
        Event::ShowRoute { route } => {
            model.route = route.clone();
            match route.as_str() {
                "pull-requests" => update(Event::LoadPullRequests, model),
                "usage" => update(Event::LoadUsage { days: if model.pages.usage_days == 0 { 7 } else { model.pages.usage_days } }, model),
                _ => render(),
            }
        }
        Event::ToggleProject { project_id } => {
            let collapsed = &mut model.sidebar_layout.collapsed_projects;
            if !collapsed.remove(&project_id) {
                collapsed.insert(project_id);
            }
            save_sidebar_layout(model).and(render())
        }
        Event::SetSidebarGrouping { grouping } => {
            let Some(grouping) = crate::sidebar::Grouping::parse(&grouping) else { return Command::done() };
            model.sidebar_layout.grouping = grouping;
            save_sidebar_layout(model).and(render())
        }
        Event::ToggleSection { section } => {
            let Some(section) = crate::sidebar::Section::parse(&section) else { return Command::done() };
            let collapsed = &mut model.sidebar_layout.collapsed_sections;
            if !collapsed.remove(&section) {
                collapsed.insert(section);
            }
            save_sidebar_layout(model).and(render())
        }
        Event::ShowAllInSection { section } => {
            let Some(section) = crate::sidebar::Section::parse(&section) else { return Command::done() };
            if !model.show_all_sections.remove(&section) {
                model.show_all_sections.insert(section);
            }
            render()
        }
        Event::SidebarLayoutLoaded { value } => {
            // A layout saved by another version that does not read keeps the default.
            if let Some(layout) = value.and_then(|v| serde_json::from_str(&v).ok()) {
                model.sidebar_layout = layout;
            }
            render()
        }
        Event::ShowAllThreads { project_id } => {
            if !model.show_all_projects.remove(&project_id) {
                model.show_all_projects.insert(project_id);
            }
            render()
        }
        Event::SetSearch { query } => {
            model.search.query = query.clone();
            model.search.revision += 1;
            if query.trim().chars().count() < 2 {
                model.search.results.clear();
                model.search.searching = false;
                return render();
            }
            model.search.searching = true;
            let rev = model.search.revision;
            call(model, requests::orchestration_search_threads(query.trim(), 20), Route::SearchThreads(rev)).and(render())
        }
        Event::RenameThread { thread_id, title } => {
            let title = title.trim().to_owned();
            if title.is_empty() {
                return Command::done();
            }
            let rename = ThreadMetadataUpdate {
                command_id: command_id(),
                thread_id: thread_id.into(),
                title: Some(title),
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_branch: None,
                expected_empty: None,
                limit_recovery: None,
                linked_pull_request: None,
            };
            dispatch(model, rename, "Rename thread")
        }
        Event::PlanForLimitReset { snooze } => {
            let Some(tid) = model.selected_thread.clone() else { return render() };
            let Some(shell) = model.thread_shell(&tid) else { return render() };
            let (Some(run), Some(reset)) = (shell.latest_run_id.clone(), shell.usage_limit_reset_at.clone().flatten()) else {
                model.toast("error", "The limit's reset time is not known.");
                return render();
            };
            let plan = ThreadMetadataUpdate {
                command_id: command_id(),
                thread_id: tid.as_str().into(),
                title: None,
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_branch: None,
                expected_empty: None,
                limit_recovery: Some(Some(domovoi_model::typed::command::LimitRecoveryUpdate { run_id: run, reset_at: reset, auto_resume: Some(!snooze), snooze: Some(snooze) })),
                linked_pull_request: None,
            };
            dispatch(model, plan, if snooze { "Snooze until reset" } else { "Resume at reset" })
        }
        Event::CancelLimitPlan => {
            let Some(tid) = model.selected_thread.clone() else { return render() };
            let cancel = ThreadMetadataUpdate {
                command_id: command_id(),
                thread_id: tid.as_str().into(),
                title: None,
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_branch: None,
                expected_empty: None,
                limit_recovery: Some(None),
                linked_pull_request: None,
            };
            dispatch(model, cancel, "Cancel")
        }
        Event::PinThread { thread_id, pinned } => {
            if pinned {
                dispatch(model, ThreadPin { command_id: command_id(), thread_id: thread_id.into(), order_key: None }, "Pin thread")
            } else {
                dispatch(model, ThreadUnpin { command_id: command_id(), thread_id: thread_id.into() }, "Pin thread")
            }
        }
        Event::SettleThread { thread_id, settled } => {
            if settled {
                let settle = ThreadSettle { command_id: command_id(), thread_id: thread_id.into(), expected_latest_run_id: None, settled_at: Some(now(model)) };
                dispatch(model, settle, "Settle thread")
            } else {
                dispatch(model, ThreadUnsettle { command_id: command_id(), thread_id: thread_id.into(), reason: Some(ByUser::User) }, "Settle thread")
            }
        }
        Event::SnoozeThread { thread_id, until_ms } => {
            if until_ms <= 0 {
                return dispatch(model, ThreadUnsnooze { command_id: command_id(), thread_id: thread_id.into(), reason: Some(ByUser::User) }, "Wake thread");
            }
            let snooze = ThreadSnooze { command_id: command_id(), thread_id: thread_id.as_str().into(), snoozed_until: iso_from_millis(until_ms).into() };
            let c = dispatch(model, snooze, "Snooze thread");
            let title = model.thread_shell(&thread_id).map_or("Thread", |t| t.title.as_str()).to_owned();
            model.toast_with_action("info", format!("Snoozed “{title}”."), "Undo", &format!("unsnooze:{thread_id}"));
            c.and(render())
        }
        Event::MarkThreadUnread { thread_id } => dispatch(model, ThreadMarkUnread { command_id: command_id(), thread_id: thread_id.into() }, "Mark unread"),
        Event::SetUnreadOnly { on } => {
            model.unread_only = on;
            render()
        }
        Event::ArchiveThread { thread_id, archived } => {
            let c = if archived {
                dispatch(model, ThreadArchive { command_id: command_id(), thread_id: thread_id.as_str().into() }, "Archive thread")
            } else {
                dispatch(model, ThreadUnarchive { command_id: command_id(), thread_id: thread_id.as_str().into() }, "Archive thread")
            };
            let refresh = call(model, requests::orchestration_get_archived_shell_snapshot(), Route::Archived);
            if archived {
                let title = model.thread_shell(&thread_id).map_or("Thread", |t| t.title.as_str()).to_owned();
                model.toast_with_action("info", format!("Archived “{title}”."), "Undo", &format!("unarchive:{thread_id}"));
            }
            if archived && model.selected_thread.as_deref() == Some(thread_id.as_str()) {
                return c.and(refresh).and(start_new_thread(model, None));
            }
            c.and(refresh).and(render())
        }
        Event::DeleteThread { thread_id } => {
            let c = dispatch(model, ThreadDelete { command_id: command_id(), thread_id: thread_id.as_str().into(), only_if_unused: None }, "Delete thread");
            if model.selected_thread.as_deref() == Some(thread_id.as_str()) {
                return c.and(start_new_thread(model, None));
            }
            c
        }
        Event::AddProject { path } => {
            let path = path.trim().trim_end_matches(['/', '\\']).to_owned();
            if path.is_empty() {
                return Command::done();
            }
            let title = crate::tools::file_name(&path).to_owned();
            let create = ProjectChange::Create { project: &ProjectId::from(uuid()), title: &title, workspace_root: &path };
            call(model, requests::projects_mutate(&uuid(), create), Route::ProjectCreated)
        }
        Event::CreateProject { name } => {
            if name.trim().is_empty() {
                return Command::done();
            }
            call(model, requests::projects_create_new(name.trim()), Route::ProjectCreated)
        }
        Event::LoadProjectIcon { project_id } => crate::project_icons::load(model, project_id),
        Event::LoadDiagram { key, source, dark } => crate::diagrams::load(model, key, source, dark),
        Event::SearchProjectIconFiles { project_id, query } => crate::project_icons::search(model, project_id, query),
        Event::LoadProjectIconThumbnail { project_id, path } => crate::project_icons::thumbnail(model, project_id, path),
        Event::CloseProjectIconFiles { project_id } => crate::project_icons::close(model, &project_id),
        Event::SetProjectIconFile { project_id, path } => crate::project_icons::save(model, project_id, None, Some(path)),
        Event::SetProjectIcon { project_id, kind, text, symbol, color } => {
            use domovoi_model::typed::{ProjectIcon, MonogramProjectIcon, EmojiProjectIcon, LucideProjectIcon};
            use unicode_segmentation::UnicodeSegmentation;
            let text = text.trim();
            let color = if crate::present::PROJECT_COLORS.contains(&color.as_str()) { color.as_str().into() } else { return Command::done(); };
            let icon = match kind.as_str() {
                "" => None,
                "monogram" if !text.is_empty() && text.graphemes(true).count() <= 2 && text.chars().next().is_some_and(char::is_alphanumeric) && text.encode_utf16().count() <= 32 && text.chars().all(|c| c.is_alphanumeric() || unicode_normalization::char::is_combining_mark(c) || matches!(c, '\u{200c}' | '\u{200d}')) =>
                    Some(ProjectIcon::Monogram(MonogramProjectIcon { text: text.into(), color })),
                "emoji" if text.graphemes(true).count() == 1 && text.encode_utf16().count() <= 32 => Some(ProjectIcon::Emoji(EmojiProjectIcon { emoji: text.into() })),
                "symbol" if !symbol.is_empty() && symbol.len() <= 64 && symbol.split('-').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())) =>
                    Some(ProjectIcon::Lucide(LucideProjectIcon { name: symbol, color, monogram: None, monogram_text: None })),
                _ => return Command::done(),
            };
            crate::project_icons::save(model, project_id, icon, None)
        }
        Event::RenameProject { project_id, title } => {
            let rename = ProjectChange::Rename { project: &project_id.into(), title: title.trim() };
            call(model, requests::projects_mutate(&uuid(), rename), Route::Command("Rename project".into()))
        }
        Event::RemoveProject { project_id } => {
            if model.selected_project.as_deref() == Some(project_id.as_str()) {
                model.selected_project = None;
            }
            call(model, requests::projects_mutate(&uuid(), ProjectChange::Delete { project: &project_id.into() }), Route::Command("Remove project".into()))
        }
        Event::OpenScratch => call(model, requests::projects_ensure_scratch(), Route::ProjectCreated),
        Event::PromoteScratchThread { thread_id } => call(model, requests::projects_create_from_thread(&thread_id.into()), Route::Command("Make project".into())),
        Event::ScanAgentSessions => {
            if model.sessions.scanning {
                return Command::done();
            }
            model.sessions.scanning = true;
            model.sessions.error = None;
            call(model, requests::agent_sessions_scan(), Route::AgentScan)
        }
        Event::ImportAgentSessions { paths } => {
            if model.sessions.importing() || paths.is_empty() {
                return Command::done();
            }
            let queue: Vec<(String, Option<String>)> = paths
                .into_iter()
                .map(|path| {
                    let existing = model.sessions.candidates.iter().find(|c| c.path == path).and_then(|c| c.project_id.as_ref()).map(|id| id.as_str().to_owned());
                    let existing = existing.or_else(|| model.projects.iter().find(|p| p.workspace_root == path).map(|p| p.id.as_str().to_owned()));
                    (path, existing)
                })
                .collect();
            let s = &mut model.sessions;
            s.total = queue.len() as u32;
            s.done = 0;
            s.imported_sessions = 0;
            s.new_projects = 0;
            s.failures.clear();
            s.queue = queue;
            import_next(model)
        }
        Event::BrowseFolders { path } => {
            let b = &mut model.browser;
            b.revision += 1;
            b.loading = true;
            b.error = None;
            // A trailing separator lists the folder's children (`\` on a
            // Windows server's paths).
            let path = match path.trim() {
                "" => "~".to_owned(),
                p => crate::tools::trim_separators(p).to_owned(),
            };
            b.path = path.clone();
            let rev = b.revision;
            let partial = if path.ends_with(crate::tools::SEPARATORS) {
                path
            } else {
                let sep = if crate::tools::is_windows_path(&path) { '\\' } else { '/' };
                format!("{path}{sep}")
            };
            call(model, requests::filesystem_browse(&partial), Route::Browse(rev))
        }

        // ---- thread ----
        Event::Respond { request_id, decision } => {
            let Some(tid) = request_thread(model, &request_id) else { return Command::done() };
            dispatch(model, respond(&tid, &request_id, &decision, None), "Respond")
        }
        Event::RespondUserInput { request_id, answers } => {
            let Some(tid) = request_thread(model, &request_id) else { return Command::done() };
            let answers = answers.into_iter().filter(|a| !a.values.is_empty()).map(|a| (a.question_id, a.values)).collect();
            dispatch(model, respond(&tid, &request_id, "accept", Some(answers)), "Answer")
        }
        Event::DismissUserInput { request_id } => {
            let Some(tid) = request_thread(model, &request_id) else { return Command::done() };
            dispatch(model, ThreadUserInputDismiss { command_id: command_id(), thread_id: tid.into(), request_id: request_id.into() }, "Skip question")
        }
        Event::AnswerSecret { item_id, value } => {
            if value.trim().is_empty() {
                return Command::done();
            }
            answer_secret(model, item_id, requests::SecretAnswer::Save(&value))
        }
        Event::DeclineSecret { item_id } => answer_secret(model, item_id, requests::SecretAnswer::Decline),
        Event::Interrupt => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            model.composer.stop_asked_ms = model.now_ms;
            model.composer.stop_asked_run = model.thread_shell(&tid).and_then(|t| t.active_run_id.as_ref()).map(|r| r.as_str().to_owned()).unwrap_or_default();
            dispatch(model, ThreadStop { command_id: command_id(), thread_id: tid.into(), reason: None, stop_delegated_tasks: None }, "Stop")
        }
        Event::Steer => {
            model.composer.steer_next = true;
            let cmd = send_message(model);
            if !model.composer.sending && model.composer.pending_send.is_none() {
                model.composer.steer_next = false;
            }
            cmd
        }
        Event::CancelQueued { run_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            dispatch(model, QueuedRunCancel { command_id: command_id(), thread_id: tid.into(), run_id: run_id.into() }, "Remove queued message")
        }
        Event::EditQueued { run_id, text } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            if text.trim().is_empty() {
                return dispatch(model, QueuedRunCancel { command_id: command_id(), thread_id: tid.into(), run_id: run_id.into() }, "Remove queued message");
            }
            let edit = QueuedRunEdit { context: None, command_id: command_id(), thread_id: tid.into(), run_id: run_id.into(), text: text.trim().to_owned(), attachments: None };
            dispatch(model, edit, "Edit queued message")
        }
        Event::SteerQueued { run_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(target) = model.thread_shell(&tid).and_then(|t| steerable_run(model, t)) else { return Command::done() };
            let promote = QueuedMessagePromoteToSteer { command_id: command_id(), thread_id: tid.into(), queued_run_id: run_id.into(), target_run_id: target.into() };
            dispatch(model, promote, "Send now")
        }
        Event::MoveQueued { run_id, delta } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(page) = crate::present::queued_order(model) else { return Command::done() };
            let Some(at) = page.iter().position(|r| *r == run_id) else { return Command::done() };
            let rest: Vec<&String> = page.iter().filter(|r| **r != run_id).collect();
            let to = (at as i64 + delta as i64).clamp(0, rest.len() as i64) as usize;
            if to == at {
                return Command::done();
            }
            let before_run_id = rest.get(to).map(|r| RunId::from(r.as_str()));
            dispatch(model, QueuedRunReorder { command_id: command_id(), thread_id: tid.into(), run_id: run_id.into(), before_run_id }, "Move queued message")
        }
        Event::ResumeQueue => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            dispatch(model, QueueResume { command_id: command_id(), thread_id: tid.into() }, "Resume queue")
        }
        Event::ForkThread { thread_id } => {
            let target = uuid();
            model.open_when_created = Some(target.clone());
            let title = model.thread_shell(&thread_id).map(|t| format!("{} (fork)", t.title));
            let fork = ThreadFork {
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                command_id: command_id(),
                source_thread_id: thread_id.into(),
                target_thread_id: target.into(),
                source_point: ForkSourcePoint::LatestStable,
                title,
                side_chat: None,
                created_at: Some(now(model)),
            };
            dispatch(model, fork, "Fork thread")
        }
        Event::ForkFromItem { item_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(run_id) = model.thread.as_ref().and_then(|t| t.turn_items.get(item_id.as_str())).and_then(|item| item.run_id().cloned()) else {
                return Command::done();
            };
            let target = uuid();
            model.open_when_created = Some(target.clone());
            let title = model.thread_shell(&tid).map(|t| format!("{} (fork)", t.title));
            let fork = ThreadFork {
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                command_id: command_id(),
                source_thread_id: tid.into(),
                target_thread_id: target.into(),
                source_point: ForkSourcePoint::Run { run_id },
                title,
                side_chat: None,
                created_at: Some(now(model)),
            };
            dispatch(model, fork, "Fork thread")
        }
        Event::MergeBack => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(parent) = model.thread_shell(&tid).and_then(|t| t.lineage.parent_thread_id.as_ref()).map(|parent| parent.as_str().to_owned()) else { return Command::done() };
            let merge = ThreadMergeBack {
                created_by: Actor::User,
                creation_source: CreationSource::Web,
                command_id: command_id(),
                source_thread_id: tid.into(),
                target_thread_id: parent.into(),
                source_point: ForkSourcePoint::LatestStable,
                created_at: Some(now(model)),
            };
            model.toast("success", "The fork's work goes to the original thread with your next message there.");
            dispatch(model, merge, "Merge back")
        }
        Event::StopSubagent { subagent_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            // A row's id is its turn item's; the subagent is the item's.
            let item = model.thread.as_ref().and_then(|thread| thread.turn_items.get(subagent_id.as_str()));
            let run = item.and_then(TurnItem::run_id).cloned();
            let id = match item {
                Some(TurnItem::Subagent(of)) => of.subagent_id.as_str().to_owned(),
                _ => subagent_id,
            };
            // The run is the row's, where the row is held and names one.
            dispatch(model, SubagentStop { command_id: command_id(), thread_id: tid.into(), run_id: run, subagent_id: id.into() }, "Stop subagent")
        }
        Event::ImplementPlan { plan_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let mut cmds = vec![];
            // Out of plan mode, or the agent would only plan again.
            if composer_interaction_mode(model) == "plan" {
                let leave = ThreadInteractionModeSet { command_id: command_id(), thread_id: tid.as_str().into(), interaction_mode: InteractionMode::Default };
                cmds.push(dispatch(model, leave, "Change mode"));
            }
            let implement = MessageDispatch {
                model_selection: current_model(model).map(|opt| model_selection(model, &opt)),
                source_plan_ref: Some(SourcePlanRef { thread_id: tid.as_str().into(), plan_id: plan_id.into() }),
                ..MessageDispatch::new(command_id(), tid.as_str(), uuid(), "Implement the plan.", Actor::User, CreationSource::Web)
            };
            cmds.push(dispatch(model, implement, "Implement plan"));
            Command::all(cmds).and(render())
        }
        Event::SavePlan { item_id, path } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(cwd) = model.thread_cwd(&tid) else {
                model.toast("error", "This thread has no folder to save to.");
                return render();
            };
            let markdown = model.thread.as_ref().and_then(|t| t.turn_items.get(item_id.as_str())).and_then(|item| match item {
                TurnItem::ProposedPlan(plan) => Some(plan.markdown.clone()),
                _ => None,
            });
            let Some(markdown) = markdown else { return Command::done() };
            let path = match path.trim().trim_start_matches('/') {
                "" => plan_file_name(&markdown),
                given => given.to_owned(),
            };
            let request = requests::projects_write_file(&cwd, &path, &markdown);
            call(model, request, Route::Command("Save plan".into()))
        }
        Event::Compact => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let compact = MessageDispatch {
                model_selection: current_model(model).map(|opt| model_selection(model, &opt)),
                ..MessageDispatch::new(command_id(), tid.as_str(), uuid(), "/compact", Actor::User, CreationSource::Web)
            };
            dispatch(model, compact, "Compact context").and(render())
        }
        Event::RegenerateTitle { thread_id } => {
            let regenerate = ThreadMetadataUpdate {
                command_id: command_id(),
                thread_id: thread_id.into(),
                title: None,
                regenerate_title: Some(true),
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_branch: None,
                expected_empty: None,
                limit_recovery: None,
                linked_pull_request: None,
            };
            dispatch(model, regenerate, "Rename thread")
        }
        Event::Revert { checkpoint_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let scope = model.thread.as_ref().and_then(|s| crate::present::checkpoints(s).into_iter().find(|c| c.id == checkpoint_id)).map(|c| c.scope_id).unwrap_or_default();
            let rollback = CheckpointRollback { restore_files: Some(true), command_id: command_id(), thread_id: tid.into(), scope_id: scope.into(), checkpoint_id: checkpoint_id.into() };
            dispatch(model, rollback, "Revert")
        }
        Event::ForgetThreadPermissions { thread_id } => {
            call(model, requests::orchestration_forget_thread_permissions(&thread_id), Route::ThreadPermissionsForgotten)
        }
        Event::SetThreadRuntimeMode { thread_id, mode } => {
            dispatch(model, ThreadRuntimeModeSet { command_id: command_id(), thread_id: thread_id.into(), runtime_mode: mode.as_str().into() }, "Change permissions")
        }
        Event::SetRuntimeMode { mode } => match model.selected_thread.clone() {
            Some(tid) => dispatch(model, ThreadRuntimeModeSet { command_id: command_id(), thread_id: tid.into(), runtime_mode: mode.as_str().into() }, "Change permissions"),
            None => {
                model.composer.runtime_mode = mode;
                render()
            }
        },
        Event::SetInteractionMode { mode } => match model.selected_thread.clone() {
            Some(tid) => dispatch(model, ThreadInteractionModeSet { command_id: command_id(), thread_id: tid.into(), interaction_mode: mode.as_str().into() }, "Change mode"),
            None => {
                model.composer.interaction_mode = mode;
                render()
            }
        },
        Event::FindInThread { query } => {
            model.find.open = true;
            model.find.query = query.clone();
            model.find.revision += 1;
            if query.trim().is_empty() {
                model.find.result = None;
                return render();
            }
            let Some(tid) = model.selected_thread.clone() else { return render() };
            let rev = model.find.revision;
            call(model, requests::orchestration_search_thread(&tid.into(), &query, None), Route::FindInThread(rev)).and(render())
        }
        Event::FindNext { forward } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let Some(found) = model.find.result.as_ref().filter(|found| found.total_matches > 0) else { return Command::done() };
            let step = FindStep { forward, from: found.active_match.as_ref() };
            let request = requests::orchestration_search_thread(&tid.into(), &model.find.query, Some(step));
            model.find.revision += 1;
            let rev = model.find.revision;
            call(model, request, Route::FindInThread(rev))
        }
        Event::CloseFind => {
            model.find = Default::default();
            render()
        }

        // ---- composer ----
        Event::SetDraft { text } => set_draft(model, text),
        Event::SelectModel { instance_id, model: slug } => {
            let Some((_, opt)) = model_record(model, &instance_id, &slug) else { return Command::done() };
            model.composer.model_query.clear();
            model.composer.show_hidden_models = false;
            match model.selected_thread.clone() {
                Some(tid) => {
                    let set = ThreadModelSelectionSet { command_id: command_id(), thread_id: tid.into(), model_selection: model_selection(model, &opt) };
                    dispatch(model, set, "Change model")
                }
                None => {
                    model.composer.model = Some(opt);
                    render()
                }
            }
        }
        Event::SetModelQuery { query } => {
            if model.composer.model_query == query {
                return Command::done();
            }
            model.composer.model_query = query;
            render()
        }
        Event::SetShowHiddenModels { show } => {
            model.composer.show_hidden_models = show;
            render()
        }
        Event::SetModelFavorite { instance_id, model: slug, favorite } => set_model_favorite(model, instance_id, slug, favorite),
        Event::SetModelHidden { instance_id, model: slug, hidden } => set_model_hidden(model, instance_id, slug, hidden),
        Event::SelectEffort { value } => {
            let Some(opt) = current_model(model) else { return Command::done() };
            let key = effort_key(model, &opt);
            model.composer.efforts.insert(key, value);
            match model.selected_thread.clone() {
                Some(tid) => {
                    let set = ThreadModelSelectionSet { command_id: command_id(), thread_id: tid.into(), model_selection: model_selection(model, &opt) };
                    dispatch(model, set, "Change effort").and(render())
                }
                None => render(),
            }
        }
        Event::SetModelOption { id, value } => {
            let Some(opt) = current_model(model) else { return Command::done() };
            if !trait_descriptors(model, &opt).iter().any(|d| d.id() == Some(id.as_str())) {
                return Command::done();
            }
            let key = trait_key(model, &opt, &id);
            model.composer.efforts.insert(key, value);
            match model.selected_thread.clone() {
                Some(tid) => {
                    let set = ThreadModelSelectionSet { command_id: command_id(), thread_id: tid.into(), model_selection: model_selection(model, &opt) };
                    dispatch(model, set, "Change model options").and(render())
                }
                None => render(),
            }
        }
        Event::SetWorkspaceMode { mode } => {
            model.composer.workspace_mode = mode;
            if model.composer.base_branch.is_none() {
                model.composer.base_branch = model.panel.git.local.as_ref().and_then(|local| local.ref_name.clone());
            }
            render()
        }
        Event::SetBaseBranch { branch } => {
            model.composer.base_branch = Some(branch);
            render()
        }
        Event::AddAttachment { name, mime_type, size_bytes, data_url } => {
            if !mime_type.starts_with("image/") {
                model.toast("error", format!("{name} is not an image."));
                return render();
            }
            if size_bytes > 10 * 1024 * 1024 {
                model.toast("error", format!("{name} is larger than 10 MB."));
                return render();
            }
            model.composer.attachments.push(Attachment { local_id: uuid(), name, mime_type, size_bytes, data_url, id: None, uploading: false });
            render()
        }
        Event::RemoveAttachment { id } => {
            model.composer.attachments.retain(|a| a.local_id != id);
            render()
        }
        Event::MoveMention { delta } => {
            if let Some(m) = model.composer.mention.as_mut() {
                let n = m.results.len().max(1) as i32;
                m.highlighted = ((m.highlighted as i64 + i64::from(delta)).rem_euclid(i64::from(n))) as usize;
            }
            render()
        }
        Event::PickSlashCommand { name } => {
            model.composer.draft_revision += 1;
            let name = name.trim_start_matches('/');
            // The app's own commands that take nothing run as soon as picked.
            if matches!(name, "plan" | "fork") || (name == "model" && !model.draft().contains(' ')) {
                return run_builtin(model, name, "");
            }
            set_draft(model, format!("/{name} "))
        }
        Event::PickSkill { name } => {
            let draft = model.draft();
            let Some(start) = skill_query(&draft).map(|(start, _)| start) else { return Command::done() };
            model.composer.draft_revision += 1;
            set_draft(model, format!("{}${} ", &draft[..start], name.trim_start_matches('$')))
        }
        Event::RecallPrompt { delta } => {
            let Some(thread) = model.thread.as_ref() else { return Command::done() };
            // The thread's own messages, latest first.
            let sent: Vec<String> = thread.messages.iter().rev().filter(|m| m.role == MessageRole::User && !m.text.trim().is_empty()).map(|m| m.text.clone()).collect();
            let (at, before) = model.composer.recall.clone().unwrap_or((0, model.draft()));
            // Typing over a recalled message makes it the draft.
            if at > 0 && sent.get(at - 1).is_some_and(|recalled| *recalled != model.draft()) {
                model.composer.recall = None;
                return Command::done();
            }
            let next = if delta < 0 { (at + 1).min(sent.len()) } else { at.saturating_sub(1) };
            if next == at {
                return Command::done();
            }
            let text = if next == 0 { before.clone() } else { sent[next - 1].clone() };
            model.composer.recall = (next > 0).then_some((next, before));
            replace_draft(model, text);
            model.changes.composer();
            render()
        }
        Event::StashDraft => {
            let text = model.draft();
            if text.trim().is_empty() {
                return Command::done();
            }
            model.composer.stash.retain(|s| *s != text);
            model.composer.stash.insert(0, text);
            model.composer.stash.truncate(20);
            replace_draft(model, String::new());
            model.drafts_dirty = true;
            model.toast("success", "Draft put aside. Find it under Stash.");
            render()
        }
        Event::RestoreStash { index } => {
            if (index as usize) >= model.composer.stash.len() {
                return Command::done();
            }
            let text = model.composer.stash.remove(index as usize);
            // What was typed goes aside in its place.
            let current = model.draft();
            if !current.trim().is_empty() {
                model.composer.stash.insert(0, current);
            }
            replace_draft(model, text);
            model.drafts_dirty = true;
            render()
        }
        Event::DropStash { index } => {
            if (index as usize) < model.composer.stash.len() {
                model.composer.stash.remove(index as usize);
                model.drafts_dirty = true;
            }
            render()
        }
        Event::ForceStop => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            model.composer.stop_asked_run.clear();
            call(model, requests::orchestration_force_stop_thread(&tid.as_str().into()), Route::Command("Force stop".into())).and(render())
        }
        Event::DismissMention => {
            model.composer.mention_dismissed_at = model.draft().rfind('@');
            if model.composer.mention.take().is_some() { render() } else { Command::done() }
        }
        Event::PickMention { path } => {
            let draft = model.draft();
            let path = if path.is_empty() {
                match model.composer.mention.as_ref().and_then(|m| m.results.get(m.highlighted)) {
                    Some(e) => e.path.clone(),
                    None => return Command::done(),
                }
            } else {
                path
            };
            if let Some(at) = draft.rfind('@') {
                replace_draft(model, format!("{}@{} ", &draft[..at], path));
            }
            model.composer.mention = None;
            render()
        }
        Event::Send => send_message(model),
        Event::Retry => {
            let last = model.thread.as_ref().and_then(|t| t.messages.iter().rev().find(|m| m.role == MessageRole::User)).map(|m| m.text.clone());
            match last.filter(|t| !t.trim().is_empty()) {
                Some(text) if model.selected_thread.is_some() => {
                    let key = model.draft_key();
                    model.composer.drafts.insert(key, text);
                    send_message(model)
                }
                _ => Command::done(),
            }
        }

        // ---- panel ----
        Event::TogglePanel => {
            model.panel.open = !model.panel.open;
            if model.panel.tab.is_empty() {
                model.panel.tab = "diff".into();
            }
            if model.panel.open { load_panel_tab(model).and(render()) } else { render() }
        }
        Event::ShowPanel { tab } => {
            let reload = !model.panel.open || model.panel.tab != tab;
            model.panel.open = true;
            model.panel.tab = tab;
            if reload { load_panel_tab(model).and(render()) } else { render() }
        }
        Event::SelectDiffSource { source } => {
            model.panel.diff.source = source;
            load_diff(model).and(render())
        }
        Event::NewTerminal => new_terminal(model).and(render()),
        Event::TerminalSplitsLoaded { value } => {
            if let Some(splits) = value.and_then(|value| serde_json::from_str(&value).ok()) {
                model.panel.terminal_splits = splits;
            }
            render()
        }
        Event::SetTerminalSplit { mode } => {
            let (Some(thread), Some(environment)) = (model.selected_thread.clone(), model.active_env.clone()) else { return Command::done() };
            let at = model.panel.terminal_splits.iter().position(|split| split.thread == thread && split.environment == environment);
            if mode == "single" {
                let Some(at) = at else { return Command::done() };
                model.panel.terminal_splits.remove(at);
                let hidden = model.panel.terminals.iter().find(|terminal| Some(terminal.id.as_str()) != model.panel.active_terminal.as_deref())
                    .map(|terminal| terminal.id.clone());
                let detach = hidden.map_or_else(Command::done, |id| set_terminal_watch(model, &id, true));
                return detach.and(save_terminal_splits(model));
            }
            let stacked = match mode.as_str() {
                "stacked" => true,
                "sideBySide" => false,
                _ => return Command::done(),
            };
            if let Some(at) = at {
                model.panel.terminal_splits[at].stacked = stacked;
            } else {
                model.panel.terminal_splits.push(terminal::SplitPreference { environment, thread, stacked, percent: 50 });
            }
            if model.panel.terminals.len() < 2 {
                new_terminal(model).and(save_terminal_splits(model))
            } else {
                save_terminal_splits(model)
            }
        }
        Event::ResizeTerminalSplit { percent } => {
            let (Some(thread), Some(environment)) = (model.selected_thread.as_deref(), model.active_env.as_deref()) else { return Command::done() };
            let Some(split) = model.panel.terminal_splits.iter_mut().find(|split| split.thread == thread && split.environment == environment) else { return Command::done() };
            let percent = percent.clamp(15, 85);
            if split.percent == percent { return Command::done() }
            split.percent = percent;
            save_terminal_splits(model)
        }
        Event::ReopenTerminal => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.as_deref()) else { return Command::done() };
            let Some(cwd) = model.panel.terminals.iter().find(|session| session.id == id && !session.running())
                .map(|session| session.cwd.clone()).filter(|cwd| !cwd.is_empty()) else { return Command::done() };
            if model.panel.terminal_details_available {
                let new_id = format!("term-{}", uuid());
                let preference = terminal_preference_key(model, &thread, &new_id);
                let request = requests::terminal_reopen(&thread.into(), id, &new_id);
                model.panel.interactive_terminals.insert(preference);
                model.panel.focus_terminal = Some(new_id);
                return call(model, request, Route::Command("Open fresh shell".into()));
            }
            new_terminal_in(model, &thread, &cwd).and(render())
        }
        Event::RenameTerminal { label } => {
            if !model.panel.terminal_details_available { return Command::done(); }
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            call(model, requests::terminal_rename(&thread.into(), &id, &label), Route::Command("Rename terminal".into()))
        }
        Event::OpenTerminalOnEnvironment { environment_id, thread_id, terminal_id } => {
            if !model.environments.iter().any(|environment| environment.id == environment_id) {
                return Command::done();
            }
            if model.active_env.as_ref() == Some(&environment_id) && model.connection == Connection::Connected {
                if model.panel.terminal_details.iter().any(|terminal| terminal.thread_id == thread_id && terminal.terminal_id == terminal_id) {
                    return update(Event::OpenTerminalAttention { thread_id, terminal_id }, model);
                }
                model.pending_terminal_target = Some((environment_id, thread_id, terminal_id));
                return Command::done();
            }
            let select = crate::environments::select(model, environment_id.clone(), None);
            model.pending_terminal_target = Some((environment_id, thread_id, terminal_id));
            select
        }
        Event::OpenTerminalAttention { thread_id, terminal_id } => {
            let Some(detail) = model.panel.terminal_details.iter().find(|terminal| terminal.thread_id == thread_id && terminal.terminal_id == terminal_id) else { return Command::done() };
            if let Some(agent) = &detail.agent {
                model.panel.terminal_seen.insert(detail.instance_id.clone(), agent.revision);
            }
            let key = terminal_preference_key(model, &thread_id, &terminal_id);
            model.panel.interactive_terminals.remove(&key);
            let selected = select_thread(model, thread_id.clone());
            model.panel.open = true;
            model.panel.tab = "terminal".into();
            model.panel.focus_terminal = Some(terminal_id.clone());
            if let Some(session) = model.panel.terminals.iter_mut().find(|session| session.id == terminal_id) {
                model.panel.active_terminal = Some(terminal_id.clone());
                model.panel.focus_terminal = None;
                if !session.watching {
                    return selected.and(update(Event::SetTerminalWatch { watching: true }, model));
                }
            }
            selected.and(render())
        }
        Event::SetTerminalWatch { watching } => {
            let Some(id) = model.panel.active_terminal.clone() else { return Command::done() };
            set_terminal_watch(model, &id, watching)
        }
        Event::SelectTerminal { terminal_id } => {
            model.panel.active_terminal = Some(terminal_id);
            render()
        }
        Event::CloseTerminal { terminal_id } => {
            let Some(thread) = model.panel.terminal_owner.clone() else { return Command::done() };
            let route = Route::Terminal { thread_id: thread.clone(), terminal_id: terminal_id.clone() };
            let key = terminal_preference_key(model, &thread, &terminal_id);
            model.panel.interactive_terminals.remove(&key);
            let u = unsubscribe(model, &route);
            model.panel.terminals.retain(|t| t.id != terminal_id);
            if model.panel.active_terminal.as_deref() == Some(terminal_id.as_str()) {
                model.panel.active_terminal = model.panel.terminals.last().map(|t| t.id.clone());
            }
            u.and(call(model, requests::terminal_close(&thread.into(), &terminal_id), Route::Command("Close terminal".into()))).and(render())
        }
        Event::TerminalScroll { lines } => {
            let Some(id) = model.panel.active_terminal.clone() else { return Command::done() };
            let Some(t) = model.panel.terminals.iter_mut().find(|t| t.id == id) else { return Command::done() };
            t.scroll(lines);
            render()
        }
        Event::TerminalKey { key, text, ctrl, alt } => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            if model.panel.terminals.iter().any(|terminal| terminal.id == id && (terminal.watching || !terminal.running())) {
                return Command::done();
            }
            // Typing shows the live screen, where it lands.
            if let Some(t) = model.panel.terminals.iter_mut().find(|t| t.id == id) { t.scroll_to_bottom(); }
            let app_cursor = model.panel.terminals.iter().find(|t| t.id == id).is_some_and(|t| t.screen().application_cursor());
            let bytes = terminal::key_bytes(&key, &text, ctrl, alt, app_cursor);
            if bytes.is_empty() {
                return Command::done();
            }
            let data = String::from_utf8_lossy(&bytes).into_owned();
            call(model, requests::terminal_write(&thread.into(), &id, &data), Route::Command("Terminal".into()))
        }
        Event::ClearTerminal => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            if model.panel.terminals.iter().any(|terminal| terminal.id == id && (terminal.watching || !terminal.running())) {
                return Command::done();
            }
            let thread = ThreadId::from(thread);
            let clear = call(model, requests::terminal_clear(&thread, &id), Route::Command("Clear terminal".into()));
            // Ctrl-L: the shell draws its prompt again on the empty screen.
            let redraw = call(model, requests::terminal_write(&thread, &id, "\u{c}"), Route::Command("Terminal".into()));
            clear.and(redraw)
        }
        Event::TerminalPaste { text } => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            if model.panel.terminals.iter().any(|terminal| terminal.id == id && (terminal.watching || !terminal.running())) {
                return Command::done();
            }
            if let Some(t) = model.panel.terminals.iter_mut().find(|t| t.id == id) { t.scroll_to_bottom(); }
            let bracketed = model.panel.terminals.iter().find(|t| t.id == id).is_some_and(|t| t.screen().bracketed_paste());
            let data = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            call(model, requests::terminal_write(&thread.into(), &id, &data), Route::Command("Terminal".into()))
        }
        Event::TerminalResize { rows, cols } => {
            let Some(id) = model.panel.active_terminal.clone() else { return Command::done() };
            resize_terminal_pane(model, &id, rows, cols)
        }
        Event::TerminalPaneResize { terminal_id, rows, cols } => resize_terminal_pane(model, &terminal_id, rows, cols),
        Event::ToggleDirectory { path } => {
            if model.panel.files.expanded.remove(&path) {
                return render();
            }
            model.panel.files.expanded.insert(path.clone());
            if model.panel.files.children.contains_key(&path) {
                return render();
            }
            list_dir(model, &path).and(render())
        }
        Event::OpenArtifactFile { thread_id, path } => {
            if model.selected_thread.as_deref() != Some(thread_id.as_str()) {
                return Command::done();
            }
            let Some(root) = model.active_cwd() else { return Command::done() };
            let Some(path) = crate::artifacts::workspace_html_path(&root, &path) else { return Command::done() };
            let request = uuid();
            let connected = model.connection == Connection::Connected;
            model.panel.files.artifact_preview = Some(crate::model::ArtifactRead {
                request: request.clone(), thread: thread_id, root: root.clone(),
                preview: crate::model::Preview {
                    path: path.clone(), loading: connected,
                    error: (!connected).then(|| "Connect to the server to preview this file.".into()),
                    ..Default::default()
                },
            });
            if connected {
                call(model, requests::projects_read_file(&root, &path), Route::ReadArtifactFile(request)).and(render())
            } else {
                render()
            }
        }
        Event::CloseArtifactFile => {
            model.panel.files.artifact_preview = None;
            render()
        }
        Event::OpenArtifact { thread_id, artifact_id, version, dark } => {
            if model.selected_thread.as_deref() != Some(thread_id.as_str()) {
                return Command::done();
            }
            model.panel.open = true;
            model.panel.tab = "artifacts".into();
            model.panel.artifacts.thread_id = Some(thread_id);
            model.panel.artifacts.active_id = Some(artifact_id);
            model.panel.artifacts.active_version = version;
            model.panel.artifacts.dark = dark;
            if model.panel.artifacts.mode.is_empty() {
                model.panel.artifacts.mode = "preview".into();
            }
            load_active_artifact(model).and(render())
        }
        Event::OpenArtifactWorkspacePath { thread_id, path, dark } => {
            if model.selected_thread.as_deref() != Some(thread_id.as_str()) {
                return Command::done();
            }
            let Some(root) = model.active_cwd() else { return Command::done() };
            let Some(rel) = crate::artifacts::workspace_artifact_path(&root, &path) else { return Command::done() };
            let collected = crate::present::collect_artifacts_for_model(model);
            let artifact_id = collected
                .iter()
                .find(|a| a.path.as_deref() == Some(rel.as_str()))
                .map(|a| a.id.clone())
                .unwrap_or_else(|| format!("file:{rel}"));
            update(Event::OpenArtifact { thread_id, artifact_id, version: None, dark }, model)
        }
        Event::SelectArtifactVersion { version, dark } => {
            model.panel.artifacts.active_version = Some(version);
            model.panel.artifacts.dark = dark;
            load_active_artifact(model).and(render())
        }
        Event::SetArtifactViewMode { mode } => {
            model.panel.artifacts.mode = if mode == "code" { "code" } else { "preview" }.into();
            render()
        }
        Event::CloseArtifact => {
            model.panel.artifacts.active_id = None;
            model.panel.artifacts.active_version = None;
            model.panel.artifacts.file_read = None;
            model.panel.artifacts.preview_url = None;
            render()
        }
        Event::RefreshArtifactPreview { dark } => {
            model.panel.artifacts.dark = dark;
            model.panel.artifacts.preview_url = None;
            load_active_artifact(model).and(render())
        }
        Event::AskAboutArtifact { artifact_id, prompt } => {
            let collected = crate::present::collect_artifacts_for_model(model);
            let title = collected
                .iter()
                .find(|a| a.id == artifact_id)
                .map(|a| a.title.as_str())
                .unwrap_or("artifact");
            let text = if prompt.trim().is_empty() {
                format!("Update artifact \"{title}\": ")
            } else {
                format!("Regarding artifact \"{title}\": {}", prompt.trim())
            };
            let key = model.draft_key();
            let cur = model.composer.drafts.get(&key).cloned().unwrap_or_default();
            let next = if cur.trim().is_empty() { text } else { format!("{}\n\n{text}", cur.trim_end()) };
            replace_draft(model, next);
            render()
        }
        Event::SaveFile { path, text } => {
            let cwd = model.panel.files.root.clone();
            if cwd.is_empty() || path.is_empty() {
                return Command::done();
            }
            call(model, requests::projects_write_file(&cwd, &path, &text), Route::Command("Save file".into())).and(update(Event::OpenFile { path }, model))
        }
        Event::OpenFile { path } => {
            let cwd = model.panel.files.root.clone();
            let request = uuid();
            model.panel.files.preview = Some(crate::model::Preview {
                path: path.clone(), request: request.clone(), loading: true, ..Default::default()
            });
            call(model, requests::projects_preview_file(&cwd, &path), Route::ReadFile { path, request }).and(render())
        }
        Event::FetchLfsObject { path } => {
            let cwd = model.panel.files.root.clone();
            let Some(preview) = model.panel.files.preview.as_mut().filter(|p| p.path == path && p.lfs.is_some() && !p.fetching) else {
                return Command::done();
            };
            preview.fetching = true;
            preview.fetch_error = None;
            let request = preview.request.clone();
            call(model, requests::projects_fetch_lfs_object(&cwd, &path), Route::FetchLfs { path, request }).and(render())
        }
        Event::SetFileQuery { query } => {
            model.panel.files.query = query.clone();
            model.panel.files.revision += 1;
            if query.trim().is_empty() {
                model.panel.files.results.clear();
                return render();
            }
            let rev = model.panel.files.revision;
            let cwd = model.panel.files.root.clone();
            call(model, requests::projects_search_entries(&cwd, &query, 50), Route::FileSearch(rev)).and(render())
        }
        Event::CreateWorktree { base, branch } => {
            let cwd = model.panel.git.cwd.clone();
            if cwd.is_empty() || base.trim().is_empty() {
                return Command::done();
            }
            let branch = branch.trim().to_owned();
            let request = requests::vcs_create_worktree(&cwd, base.trim(), (!branch.is_empty()).then_some(branch.as_str()));
            call(model, request, Route::Command("Create worktree".into()))
        }
        Event::RemoveWorktree { path, force } => {
            let cwd = model.panel.git.cwd.clone();
            if cwd.is_empty() || path == cwd {
                model.toast("error", "This thread works in that worktree.");
                return render();
            }
            call(model, requests::vcs_remove_worktree(&cwd, &path, force), Route::Command("Remove worktree".into()))
        }
        Event::RunScript { id } => {
            let project = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)).map(|t| t.project_id.as_str().to_owned()).or_else(|| model.selected_project.clone());
            let Some(script) = project.and_then(|p| model.project(&p)).and_then(|p| p.scripts.iter().find(|s| s.id == id)).cloned() else { return Command::done() };
            let opened = new_terminal(model);
            if let Some(session) = model.panel.active_terminal.clone().and_then(|active| model.panel.terminals.iter_mut().find(|t| t.id == active)) {
                session.label = script.name.clone();
                session.pending_input = Some(format!("{}\r", script.command));
            }
            model.panel.open = true;
            model.panel.tab = "terminal".into();
            opened.and(render())
        }
        Event::RefreshGit => {
            let cwd = model.panel.git.cwd.clone();
            if cwd.is_empty() {
                return Command::done();
            }
            let a = call(model, requests::vcs_refresh_status(&cwd), Route::Command("Refresh".into()));
            let b = call(model, requests::vcs_list_refs(&cwd), Route::ListRefs(cwd));
            a.and(b)
        }
        Event::GitAction { action, message } => {
            let cwd = model.panel.git.cwd.clone();
            if cwd.is_empty() || model.panel.git.busy {
                return Command::done();
            }
            model.panel.git.busy = true;
            model.panel.git.error = None;
            model.panel.git.progress = vec![];
            let action_id = uuid();
            let message = message.trim();
            let thread = model.selected_thread.as_deref().map(ThreadId::from);
            let action = GitStackedAction::from(action.as_str());
            let request = requests::git_run_stacked_action(&action_id, &cwd, &action, (!message.is_empty()).then_some(message), thread.as_ref());
            call(model, request, Route::GitAction(action_id)).and(render())
        }
        Event::GitPull => {
            let cwd = model.panel.git.cwd.clone();
            model.panel.git.busy = true;
            call(model, requests::vcs_pull(&cwd), Route::Command("Pull".into())).and(render())
        }
        Event::SwitchBranch { name } => {
            let cwd = model.panel.git.cwd.clone();
            call(model, requests::vcs_switch_ref(&cwd, &name), Route::Command("Switch branch".into()))
        }
        Event::CreateBranch { name } => {
            let cwd = model.panel.git.cwd.clone();
            call(model, requests::vcs_create_ref(&cwd, name.trim()), Route::Command("Create branch".into()))
        }

        // ---- palette ----
        Event::OpenPalette => {
            model.palette = Some(Palette::default());
            render()
        }
        Event::ClosePalette => {
            model.palette = None;
            render()
        }
        Event::SetPaletteQuery { query } => {
            if let Some(p) = model.palette.as_mut() {
                p.query = query;
                p.highlighted = 0;
            }
            palette_search(model).and(render())
        }
        Event::SetPaletteMode { mode } => {
            if let Some(p) = model.palette.as_mut() {
                p.mode = if matches!(mode.as_str(), "files" | "content") { mode } else { String::new() };
                p.query.clear();
                p.results.clear();
                p.highlighted = 0;
                p.searching = false;
            }
            render()
        }
        Event::MovePalette { delta } => {
            let n = crate::present::palette_items(model).len().max(1) as i32;
            if let Some(p) = model.palette.as_mut() {
                p.highlighted = ((p.highlighted as i64 + i64::from(delta)).rem_euclid(i64::from(n))) as usize;
            }
            render()
        }
        Event::RunPaletteItem { id } => {
            let id = if id.is_empty() {
                let items = crate::present::palette_items(model);
                match model.palette.as_ref().and_then(|p| items.get(p.highlighted)) {
                    Some(i) => i.id.clone(),
                    None => return Command::done(),
                }
            } else {
                id
            };
            palette_command(model, &id)
        }

        // ---- settings ----
        Event::LoadSettings { section } => match section.as_str() {
            "providers" => {
                let a = call(model, requests::server_refresh_providers(false), Route::Command("Refresh providers".into()));
                let b = call(model, requests::providers_failover_status(), Route::FailoverStatus);
                a.and(b).and(provider_setup_streams(model))
            }
            "projects" => {
                let ids: Vec<String> = model.projects.iter().filter(|p| !p.workspace_root.ends_with("/scratch")).map(|p| p.id.as_str().to_owned()).collect();
                let mut cmd = Command::done();
                for pid in ids {
                    cmd = cmd.and(call(model, requests::projects_index_status(&pid), Route::ProjectIndex(pid)));
                }
                cmd.and(render())
            }
            "general" => call(model, requests::server_refresh_providers(false), Route::Command("Refresh providers".into())),
            "scheduled" => subscribe(model, requests::scheduled_tasks_subscribe(), Route::ScheduledTasks),
            "archived" => call(model, requests::orchestration_get_archived_shell_snapshot(), Route::Archived),
            "storage" => {
                let a = subscribe(model, requests::server_get_database_storage(), Route::DatabaseStorage);
                let b = subscribe(model, requests::server_get_storage_cleanup_report(), Route::CleanupReport);
                a.and(b)
            }
            "diagnostics" => {
                let a = call(model, requests::server_get_process_diagnostics(), Route::Diagnostics);
                let b = call(model, requests::server_get_host_resources(), Route::HostResources);
                a.and(b)
            }
            "access" => subscribe(model, requests::subscribe_auth_access(), Route::AuthAccess).and(crate::environments::load_remote_access(model)),
            _ => render(),
        },
        Event::SetSettingBool { key, value } => setting_patch(model, &key, Typed::Switch(value)),
        Event::SetSettingText { key, value } => {
            // A setting that shows a number takes one, if that is what was typed.
            let number = if settings::is_number(model, &key) { value.trim().parse::<i64>().ok() } else { None };
            setting_patch(model, &key, number.map_or(Typed::Text(value), Typed::Number))
        }
        Event::RefreshProviders => call(model, requests::server_refresh_providers(true), Route::Command("Refresh providers".into())),
        Event::StartProviderSignIn { instance_id } => {
            let watch = subscribe(model, requests::provider_auth_subscribe(&instance_id), Route::ProviderAuth(instance_id.clone()));
            watch.and(call(model, requests::provider_auth_start(&instance_id), Route::Command("Sign in".into())))
        }
        Event::CompleteProviderSignIn { instance_id, callback_url } => {
            let Some(flow) = model.pages.provider_auth.get(&instance_id).filter(|a| a.busy()).and_then(|a| a.flow_id.clone()) else {
                model.toast("error", "No sign-in is under way here. Start it again.");
                return render();
            };
            call(model, requests::provider_auth_complete(&instance_id, &flow, callback_url.trim()), Route::Command("Sign in".into()))
        }
        Event::CancelProviderSignIn { instance_id } => {
            let Some(flow) = model.pages.provider_auth.get(&instance_id).and_then(|a| a.flow_id.clone()) else { return render() };
            call(model, requests::provider_auth_cancel(&instance_id, &flow), Route::Command("Cancel sign-in".into()))
        }
        Event::InstallProvider { instance_id } => {
            let watch = subscribe(model, requests::provider_install_subscribe(&instance_id), Route::ProviderInstall(instance_id.clone()));
            watch.and(call(model, requests::provider_install_start(&instance_id), Route::Command("Install".into())))
        }
        Event::CancelProviderInstall { instance_id } => {
            let Some(op) = model.pages.provider_install.get(&instance_id).and_then(|i| i.operation_id.clone()) else { return render() };
            call(model, requests::provider_install_cancel(&instance_id, &op), Route::Command("Cancel install".into()))
        }
        Event::RemoveProviderInstall { instance_id } => call(model, requests::provider_install_remove(&instance_id), Route::Command("Remove runtime".into())),
        Event::SaveProviderInstance { edit } => {
            // A new instance is named after its driver, numbered past the ones there are.
            let id = if edit.instance_id.trim().is_empty() {
                let taken = |id: &str| {
                    model.providers.iter().any(|p| p.instance_id.as_str() == id)
                        || model.settings.as_ref().and_then(|s| s.provider_instances.as_ref()).is_some_and(|i| i.contains_key(&ProviderInstanceId::from(id)))
                };
                (2..).map(|n| format!("{}-{n}", edit.driver)).find(|id| !taken(id)).unwrap_or_default()
            } else {
                edit.instance_id.clone()
            };
            let existing = model.settings.as_ref().and_then(|s| s.provider_instances.as_ref()).and_then(|i| i.get(&ProviderInstanceId::from(id.as_str())));
            let request = requests::server_upsert_provider_instance(&id, &edit, existing);
            call(model, request, Route::Settings).and(call(model, requests::server_refresh_providers(true), Route::Command("Refresh providers".into())))
        }
        Event::RemoveProviderInstance { instance_id } => call(model, requests::server_remove_provider_instance(&instance_id), Route::Settings),
        Event::RunStorageCleanup => {
            model.pages.storage_busy = true;
            call(model, requests::server_run_storage_cleanup(), Route::Command("Storage cleanup".into())).and(render())
        }
        Event::ReclaimDatabase => {
            model.pages.storage_busy = true;
            call(model, requests::server_reclaim_database_space(), Route::Command("Reclaim space".into())).and(render())
        }
        Event::SetScheduledEnabled { id, enabled } => call(model, requests::scheduled_tasks_set_enabled(&id.into(), enabled), Route::Command("Scheduled task".into())),
        Event::RunScheduledNow { id } => call(model, requests::scheduled_tasks_run_now(&id.as_str().into()), Route::ScheduledRunNow(id)),
        Event::DeleteScheduled { id } => call(model, requests::scheduled_tasks_delete(&id.into()), Route::Command("Delete task".into())),
        Event::CreateScheduled { name, prompt, project_id, interval_minutes } => {
            let Some(opt) = current_model(model) else {
                model.toast("error", "No provider is ready.");
                return render();
            };
            let task = NewScheduledTask {
                title: &name,
                prompt: &prompt,
                schedule: &ScheduledTaskSchedule::Interval(IntervalSchedule { every_ms: interval_minutes.max(1) as u64 * 60_000 }),
                project: &project_id.into(),
                model_selection: &model_selection(model, &opt),
                runtime_mode: &RuntimeMode::from(composer_runtime_mode(model).as_str()),
            };
            call(model, requests::scheduled_tasks_upsert(task), Route::Command("Create task".into()))
        }
        Event::SaveScheduled { task } => {
            let Some(schedule) = edited_schedule(&task) else {
                model.toast("error", "That schedule is not complete.");
                return render();
            };
            let selection = if task.model.is_empty() {
                let Some(opt) = current_model(model) else {
                    model.toast("error", "No provider is ready.");
                    return render();
                };
                model_selection(model, &opt)
            } else {
                ModelSelection { instance_id: task.model_instance.as_str().into(), model: task.model.clone(), options: None }
            };
            let runtime = if task.runtime_mode.is_empty() { composer_runtime_mode(model) } else { task.runtime_mode.clone() };
            let interaction = if task.interaction_mode == "plan" { InteractionMode::Plan } else { InteractionMode::Default };
            let workspace = if task.workspace == "worktree" && !task.base_branch.is_empty() {
                WorkspaceStrategy::Worktree { base_ref: task.base_branch.clone(), branch: None, start_from_origin: None }
            } else {
                WorkspaceStrategy::Root { branch: None }
            };
            // One thread without one chosen: the server binds the thread its first run starts.
            let one_thread = matches!(task.thread_mode.as_str(), "continue" | "fresh_session");
            let bound = one_thread && !task.thread_id.is_empty();
            let edited = EditedScheduledTask {
                id: (!task.id.is_empty()).then_some(task.id.as_str()),
                title: &task.name,
                prompt: &task.prompt,
                enabled: task.enabled,
                schedule: &schedule,
                project: &task.project_id.as_str().into(),
                thread_mode: if one_thread { &task.thread_mode } else { "new_thread" },
                thread_id: bound.then_some(task.thread_id.as_str()),
                workspace: &workspace,
                model_selection: &selection,
                runtime_mode: &RuntimeMode::from(runtime.as_str()),
                interaction_mode: &interaction,
            };
            let label = if task.id.is_empty() { "Create task" } else { "Save task" };
            call(model, requests::scheduled_tasks_save(edited), Route::Command(label.into()))
        }
        Event::RotateWebhookToken { id } => call(model, requests::scheduled_tasks_rotate_webhook_token(&id.into()), Route::Command("New webhook URL".into())),
        // No task: the open runs close.
        Event::LoadScheduledRuns { id } if id.is_empty() => {
            model.pages.scheduled_runs = None;
            render()
        }
        Event::LoadScheduledRuns { id } => {
            model.pages.scheduled_runs = Some((id.clone(), vec![]));
            call(model, requests::scheduled_tasks_list_runs(Some(&id), None), Route::ScheduledRuns(Some(id))).and(render())
        }
        Event::LoadWebhookDeliveries { id } => {
            model.pages.webhook_deliveries = Some((id.clone(), vec![]));
            call(model, requests::scheduled_tasks_list_webhook_deliveries(&id.as_str().into()), Route::WebhookDeliveries(id)).and(render())
        }
        Event::RemoveKeybinding { command, shortcut, condition } => {
            let when = (!condition.is_empty()).then_some(condition.as_str());
            call(model, requests::server_remove_keybinding(&shortcut, &command, when), Route::Keybindings)
        }

        // ---- pull requests / usage ----
        Event::LoadPullRequests => {
            model.pages.prs_loading = true;
            model.pages.prs_error = None;
            call(model, requests::pull_requests_list(), Route::PullRequests).and(render())
        }
        Event::PullRequestAction { key, action, merge_method } => {
            // How it lands goes with a merge only.
            let method = (action == "merge").then(|| PullRequestMergeMethod::from(if merge_method.is_empty() { "merge" } else { merge_method.as_str() }));
            let done = match action.as_str() {
                "merge" => "Merged.",
                "close" => "Closed.",
                "reopen" => "Reopened.",
                "ready" => "Marked ready for review.",
                "draft" => "Turned back into a draft.",
                _ => "Done.",
            };
            let action = typed::PullRequestAction::from(action.as_str());
            pr_call(model, &key, |pull_request| requests::pull_requests_run_action(pull_request, &action, method.as_ref()), done)
        }
        Event::ReviewPullRequest { key, verdict, body } => {
            let done = match verdict.as_str() {
                "approve" => "Approved.",
                "request-changes" => "Changes requested.",
                _ => "Review sent.",
            };
            pr_call(model, &key, |pull_request| requests::pull_requests_submit_review(pull_request, &verdict, body.trim()), done)
        }
        Event::CommentOnPullRequest { key, body } => pr_call(model, &key, |pull_request| requests::pull_requests_comment(pull_request, body.trim()), "Comment posted."),
        Event::StartPullRequestThread { key } => {
            let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key) else { return Command::done() };
            let reference = e.number.to_string();
            let Some(cwd) = model.project(e.project_id.as_str()).map(|p| p.workspace_root.clone()) else {
                model.toast("error", "That pull request's project isn't on this server.");
                return render();
            };
            model.pages.pr_busy = Some(key.clone());
            call(model, requests::git_prepare_pull_request_thread(&cwd, &reference), Route::PullRequestThread(key)).and(render())
        }
        Event::SelectPullRequest { key } => {
            model.pages.pr_selected = Some(key.clone());
            model.pages.pr_detail = None;
            model.pages.pr_checks = None;
            model.pages.pr_review = None;
            model.pages.pr_review_busy = false;
            model.pages.pr_review_phase_message = None;
            let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key).cloned() else { return render() };
            let (summary, checks, review) = (requests::pull_requests_summary(&e), requests::pull_requests_checks(&e), requests::pull_requests_reviews_get(&e));
            let a = call(model, summary, Route::PullRequestDetail(key.clone()));
            let b = call(model, checks, Route::PullRequestChecks(key.clone()));
            let c = call(model, review, Route::PrReview(key.clone()));
            let d = subscribe(model, requests::pull_requests_reviews_subscribe(), Route::PrReviewStream(key));
            a.and(b).and(c).and(d).and(render())
        }
        Event::StartPrReview { key, full } => {
            let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key).cloned() else { return Command::done() };
            model.pages.pr_review_busy = true;
            model.pages.pr_review_phase_message = Some("Starting AI review…".into());
            call(model, requests::pull_requests_reviews_start(&e, full), Route::PrReviewAction { key, done: "AI review started.".into() }).and(render())
        }
        Event::CancelPrReview { run_id } => {
            let key = model.pages.pr_selected.clone().unwrap_or_default();
            call(model, requests::pull_requests_reviews_cancel(&run_id), Route::PrReviewAction { key, done: "Review cancelled.".into() }).and(render())
        }
        Event::TogglePrFinding { run_id, finding_id, selected } => {
            if let Some(review) = model.pages.pr_review.as_mut()
                && let Some(f) = review.findings.iter_mut().find(|f| f.id == finding_id)
            {
                f.selected = selected;
            }
            let key = model.pages.pr_selected.clone().unwrap_or_default();
            call(
                model,
                requests::pull_requests_reviews_set_finding_selected(&run_id, &finding_id, selected),
                Route::PrReviewAction { key, done: String::new() },
            )
            .and(render())
        }
        Event::PublishPrReview { key, run_id } => {
            let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key).cloned() else { return Command::done() };
            model.pages.pr_review_busy = true;
            model.pages.pr_review_phase_message = Some("Publishing validated findings…".into());
            call(model, requests::pull_requests_reviews_publish(&e, &run_id), Route::PrReviewAction { key, done: "Review published.".into() }).and(render())
        }
        Event::ReindexProject { project_id } => {
            model.pages.project_indexes.insert(
                project_id.clone(),
                domovoi_model::typed::ProjectIndexStatus { project_id: project_id.clone(), status: "indexing".into(), ..Default::default() },
            );
            call(model, requests::projects_reindex(&project_id, true), Route::ProjectIndex(project_id)).and(render())
        }
        Event::SaveFailoverPool { pool_id, name, instance_ids, same_driver_first, allow_cross_provider } => {
            let mut pools = model
                .settings
                .as_ref()
                .and_then(|s| s.failover_pools.clone())
                .unwrap_or_default();
            let entry = domovoi_model::typed::FailoverPoolConfig {
                id: if pool_id.is_empty() { "default".into() } else { pool_id },
                name: if name.trim().is_empty() { "Default Failover Pool".into() } else { name },
                instance_ids: instance_ids.into_iter().map(ProviderInstanceId::from).collect(),
                same_driver_first,
                allow_cross_provider,
            };
            if let Some(existing) = pools.iter_mut().find(|p| p.id == entry.id) {
                *existing = entry;
            } else {
                pools.push(entry);
            }
            let a = call(model, requests::server_update_failover_pools(&pools), Route::Settings);
            let b = call(model, requests::providers_failover_status(), Route::FailoverStatus);
            a.and(b).and(render())
        }
        Event::LoadUsage { days } => {
            let days = days.max(1);
            model.pages.usage_days = days;
            model.pages.usage_loading = true;
            model.pages.usage_error = None;
            let until = local_day(model, model.now_ms);
            let since = local_day(model, model.now_ms - (days as i64 - 1) * 86_400_000);
            let tz = if model.time_zone.is_empty() { "UTC" } else { model.time_zone.as_str() };
            call(model, requests::server_get_usage_summary(&since, &until, tz, days == 1), Route::Usage).and(render())
        }
        Event::FilterUsageProvider { provider } => {
            model.pages.usage_provider_filter = provider;
            render()
        }
        Event::RefreshUsageLimits => {
            model.pages.usage_limits_refreshing = true;
            call(model, requests::server_refresh_providers(true), Route::UsageLimits).and(render())
        }
        Event::SetTimeZone { iana, utc_offset_secs } => {
            model.time_zone = iana;
            model.utc_offset_secs = utc_offset_secs;
            Command::done()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_queries() {
        assert_eq!(mention_query("look at @src/ma").as_deref(), Some("src/ma"));
        assert_eq!(mention_query("@").as_deref(), Some(""));
        assert_eq!(mention_query("mail me@example.com"), None);
        assert_eq!(mention_query("@done with it"), None);
    }
}

/// The schedule an editor's fields describe; `None` when they say too little.
pub fn edited_schedule(task: &crate::app::ScheduledTaskEdit) -> Option<ScheduledTaskSchedule> {
    use domovoi_model::typed::{FixedTimeSchedule, OnceSchedule, WebhookSchedule};
    match task.schedule_kind.as_str() {
        "interval" | "" => Some(ScheduledTaskSchedule::Interval(IntervalSchedule { every_ms: u64::from(task.interval_minutes.max(1)) * 60_000 })),
        "fixed_time" => {
            let (h, m) = task.time_of_day.split_once(':')?;
            let ok = |s: &str, max: u8| s.parse::<u8>().is_ok_and(|v| v <= max) && (1..=2).contains(&s.len());
            if !ok(h, 23) || !ok(m, 59) || m.len() != 2 {
                return None;
            }
            let mut days: Vec<u64> = task.weekdays.iter().filter(|d| **d <= 6).map(|d| u64::from(*d)).collect();
            days.sort_unstable();
            days.dedup();
            Some(ScheduledTaskSchedule::FixedTime(FixedTimeSchedule {
                time_of_day: format!("{:0>2}:{m}", h),
                weekdays: (!days.is_empty() && days.len() < 7).then_some(days),
            }))
        }
        "once" => (!task.run_at.trim().is_empty()).then(|| ScheduledTaskSchedule::Once(OnceSchedule { run_at: task.run_at.trim().into() })),
        "webhook" => Some(ScheduledTaskSchedule::Webhook(WebhookSchedule { signature: None, max_delivery_age_minutes: None, dedupe: None, filters: None })),
        _ => None,
    }
}
