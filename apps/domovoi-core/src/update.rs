//! Event handling: user intents become RPCs, orchestration commands and
//! state changes. Server replies are handled in `replies.rs`.
//!
//! A command is the record the server decodes (`domovoi_model::typed::Command`),
//! built here field by field; every other request is made by a function of
//! `requests.rs` from what it is about. Nothing here writes JSON.

use crate::app::{Event, iso_from_millis};
use crate::model::{Attachment, Connection, Mention, Model, Outgoing, Palette};
use crate::requests::{self, DiffSource, FindStep, NewScheduledTask, ProjectChange, Request, SettingsPatch};
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

/// Asks for the URLs of the open thread's image attachments not asked yet.
pub fn request_asset_urls(model: &mut Model) -> Cmd {
    let Some(thread) = model.thread.as_ref() else { return Command::done() };
    let wanted: Vec<ImageAttachment> = thread
        .messages
        .iter()
        .flat_map(|m| &m.attachments)
        .filter_map(|a| match a {
            ChatAttachment::Image(image) if !model.asset_urls.contains_key(&image.id) => Some(image.clone()),
            _ => None,
        })
        .collect();
    let mut cmds = vec![];
    for image in wanted {
        model.asset_urls.insert(image.id.clone(), String::new());
        cmds.push(call(model, requests::assets_create_url(&image), Route::AssetUrl(image.id)));
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
    model.rpc.reset();
    model.socket_lost_flags();
}

fn ws_url(server_url: &str, surface: &str, ticket: &str) -> String {
    let base = server_url.trim_end_matches('/');
    let ws = base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let surface = if surface.is_empty() { "desktop" } else { surface };
    format!("{ws}/ws?clientSurface={surface}&orchestrationProtocol=2&wsTicket={ticket}")
}

/// Opens the RPC socket with a one-time ticket.
pub fn open_socket(model: &mut Model, ticket: &str) -> Cmd {
    model.rpc.reset();
    let url = ws_url(&model.server_url, &model.surface, ticket);
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
    model.thread_loading = true;
    // Already showing it (a reconnect): only what happened since.
    let showing = model.thread.is_some() && model.selected_thread.as_deref() == Some(thread_id) && model.thread_sequence > 0;
    let request = requests::orchestration_subscribe_thread(&thread_id.into(), showing.then_some(model.thread_sequence));
    call(model, request, route)
}

fn unsubscribe_threads(model: &mut Model, keep: Option<&str>) -> Cmd {
    let open: Vec<Route> = model.rpc_routes().into_iter().filter(|r| matches!(r, Route::Thread(t) if Some(t.as_str()) != keep)).collect();
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
        _ => Command::done(),
    }
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
    match (source.as_str(), thread) {
        ("thread", Some(t)) => {
            let to = model.thread.as_ref().map(|s| crate::present::checkpoints(s).last().map(|c| c.turn_count).unwrap_or(0)).unwrap_or(0);
            call(model, requests::orchestration_get_full_thread_diff(&t.into(), to), Route::Diff(source))
        }
        (s, Some(t)) if s.starts_with("turn:") => {
            let n: u32 = s[5..].parse().unwrap_or(1);
            call(model, requests::orchestration_get_turn_diff(&t.into(), n), Route::Diff(source))
        }
        ("branch", _) => call(model, requests::review_get_diff_preview(&cwd, DiffSource::BranchRange), Route::Diff(source)),
        _ => call(model, requests::review_get_diff_preview(&cwd, DiffSource::WorkingTree), Route::Diff(source)),
    }
}

fn new_terminal(model: &mut Model) -> Cmd {
    let Some(thread) = model.selected_thread.clone() else {
        model.toast("info", "Open a thread to use terminals.");
        return render();
    };
    let cwd = model.active_cwd().unwrap_or_default();
    model.panel.terminal_counter += 1;
    let n = model.panel.terminal_counter;
    let id = format!("term-{n}");
    model.panel.terminals.push(Session::new(&id, &format!("Terminal {n}")));
    model.panel.terminal_owner = Some(thread.clone());
    model.panel.active_terminal = Some(id.clone());
    attach_terminal(model, &thread, &id, &cwd)
}

fn attach_terminal(model: &mut Model, thread: &str, id: &str, cwd: &str) -> Cmd {
    let route = Route::Terminal { thread_id: thread.to_owned(), terminal_id: id.to_owned() };
    subscribe(model, requests::terminal_attach(&thread.into(), id, cwd, terminal::DEFAULT_COLS, terminal::DEFAULT_ROWS), route)
}

/// Detaches the current thread's terminals (they keep running on the server).
fn detach_terminals(model: &mut Model) -> Cmd {
    let routes: Vec<Route> = model.rpc_routes().into_iter().filter(|r| matches!(r, Route::Terminal { .. })).collect();
    model.panel.terminals.clear();
    model.panel.active_terminal = None;
    model.panel.terminal_owner = None;
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
        model.panel.terminals.push(Session::new(&id, &label));
        if let Some(n) = id.strip_prefix("term-").and_then(|n| n.parse::<u32>().ok()) {
            model.panel.terminal_counter = model.panel.terminal_counter.max(n);
        }
        model.panel.terminal_owner = Some(thread.clone());
        model.panel.active_terminal.get_or_insert(id.clone());
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

/// The model a new message uses: the thread's, the composer's choice, the
/// default from settings, or the first ready provider's default model.
pub fn current_model(model: &Model) -> Option<ModelOption> {
    let options = model_options(model);
    let find = |instance: &str, slug: &str| options.iter().find(|(_, o)| o.instance_id == instance && o.model == slug).map(|(_, o)| o.clone());
    if let Some(t) = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)) {
        let (i, m) = (t.model_selection.instance_id.as_str(), t.model_selection.model.as_str());
        return find(i, m).or_else(|| Some(ModelOption { instance_id: i.into(), model: m.into(), label: m.into() }));
    }
    if let Some(m) = &model.composer.model {
        return Some(m.clone());
    }
    if let Some(Some(d)) = model.settings.as_ref().map(|settings| &settings.default_model_selection)
        && let Some(o) = d.as_ref().and_then(|d| find(d.instance_id.as_str(), &d.model))
    {
        return Some(o);
    }
    let ready: Vec<&(&ServerProviderModel, ModelOption)> = options.iter().filter(|(_, o)| provider_ready(model, &o.instance_id)).collect();
    ready.iter().find(|(m, _)| m.is_default == Some(true)).or(ready.first()).map(|(_, o)| o.clone())
}

/// The effort option descriptor of a model (`effort` or `reasoningEffort`).
pub fn effort_descriptor<'a>(model: &'a Model, opt: &ModelOption) -> Option<&'a ProviderOptionDescriptor> {
    model_options(model)
        .into_iter()
        .find(|(_, o)| o == opt || (o.instance_id == opt.instance_id && o.model == opt.model))
        .and_then(|(m, _)| m.capabilities.as_ref()?.option_descriptors.as_ref()?.iter().find(|d| matches!(d.id(), Some("effort" | "reasoningEffort"))))
}

/// The effort a model runs at here: the one picked for it in this thread (or
/// this new thread), the thread's own when the thread runs that model, or the
/// model's default. Only an effort the model offers: one kept from another
/// model (`xhigh` from Codex) would go to a provider that can't take it.
pub fn current_effort(model: &Model, opt: &ModelOption) -> Option<String> {
    let d = effort_descriptor(model, opt)?;
    let offered = |v: &str| d.options().iter().any(|o| o.id == v);
    if let Some(e) = model.composer.efforts.get(&effort_key(model, opt)).filter(|e| offered(e)) {
        return Some(e.clone());
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
    d.options().iter().find(|o| o.is_default == Some(true)).or(d.options().first()).map(|o| o.id.clone())
}

/// Where an effort picked for `opt` is kept: per thread (or new thread in a
/// project), so a choice in one thread doesn't change another's.
fn effort_key(model: &Model, opt: &ModelOption) -> String {
    format!("{}/{}/{}", model.draft_key(), opt.instance_id, opt.model)
}

/// The model as a command names it, with the effort chosen for it where it has one.
fn model_selection(model: &Model, opt: &ModelOption) -> ModelSelection {
    let effort = effort_descriptor(model, opt).and_then(ProviderOptionDescriptor::id).zip(current_effort(model, opt));
    ModelSelection {
        instance_id: opt.instance_id.as_str().into(),
        model: opt.model.clone(),
        options: effort.map(|(id, effort)| vec![OptionSelection { id: id.to_owned(), value: OptionValue::Choice(effort) }]),
    }
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

fn set_draft(model: &mut Model, text: String) -> Cmd {
    model.drafts_dirty = true;
    let key = model.draft_key();
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
            if had { render() } else { Command::done() }
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
        project_id: project.into(),
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
    let out = Outgoing { request, message_id: None, route: Route::Launch(thread_id.to_owned()), label: "Start thread".into(), draft_key: key, text, sent_ms: model.now_ms };
    send_outgoing(model, out).and(render())
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
fn expire_outbox(model: &mut Model) {
    if model.connection != Connection::Connected {
        return;
    }
    let now = model.now_ms;
    let (late, keep): (Vec<Outgoing>, Vec<Outgoing>) = std::mem::take(&mut model.composer.outbox).into_iter().partition(|o| now - o.sent_ms >= SEND_TIMEOUT_MS);
    model.composer.outbox = keep;
    if late.is_empty() {
        return;
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
    if late.iter().any(|o| !o.text.is_empty()) {
        model.toast("error", "The server didn't answer, so your message is back in the composer. Check the thread list before sending it again.");
    } else {
        let labels: Vec<&str> = late.iter().map(|o| o.label.as_str()).collect();
        model.toast("error", format!("The server didn't answer: {} may not have happened.", labels.join(", ")));
    }
}

/// Saves how this device lays out the thread list.
fn save_sidebar_layout(model: &Model) -> Cmd {
    let value = serde_json::to_string(&model.sidebar_layout).expect("plain data serializes");
    crate::environments::store(crate::store::StoreOperation::Save { key: crate::sidebar::KEY_SIDEBAR_LAYOUT.into(), value })
}

pub fn select_thread(model: &mut Model, thread_id: String) -> Cmd {
    if model.selected_thread.as_deref() == Some(thread_id.as_str()) && model.route == "thread" {
        return render();
    }
    let project = model.thread_shell(&thread_id).map(|t| t.project_id.as_str().to_owned());
    let mut cmds = vec![unsubscribe_threads(model, Some(&thread_id)), detach_terminals(model)];
    model.route = "thread".into();
    model.held_section = crate::sidebar::hold_for(model, &thread_id);
    model.selected_thread = Some(thread_id.clone());
    model.drafts_dirty = true;
    if project.is_some() {
        model.selected_project = project;
    }
    model.thread = None;
    model.thread_sequence = 0;
    model.thread_gaps = 0;
    model.find = Default::default();
    model.panel.diff = Default::default();
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
                .unwrap_or_else(|| path.trim_end_matches('/').rsplit('/').next().unwrap_or(&path).to_owned());
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
    model.selected_thread = None;
    model.thread = None;
    model.thread_sequence = 0;
    model.thread_gaps = 0;
    model.find = Default::default();
    model.panel.diff = Default::default();
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

fn palette_command(model: &mut Model, id: &str) -> Cmd {
    model.palette = None;
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

fn utc_day(ms: i64) -> String {
    iso_from_millis(ms)[..10].to_owned()
}

pub fn update(event: Event, model: &mut Model) -> Cmd {
    match event {
        // ---- connection ----
        Event::Launch { surface, device_label, device_os } => crate::environments::launch(model, surface, device_label, device_os),
        // A link or a tap while saved servers load waits for them.
        Event::ConnectRemote { .. } | Event::OpenNotification { .. } | Event::RespondFromNotification { .. } | Event::AnswerFromNotification { .. } | Event::RespondToThread { .. } if model.launching => {
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
        Event::SelectEnvironment { id } => {
            if model.active_env.as_deref() == Some(id.as_str()) && model.connection == Connection::Connected {
                return Command::done();
            }
            crate::environments::select(model, id, None)
        }
        Event::ForgetEnvironment { id } => crate::environments::forget(model, id),
        Event::SignedOut { id } => crate::environments::signed_out(model, id),
        Event::RenameEnvironment { id, label } => crate::environments::rename(model, id, label),
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
        Event::ExpandItem { id } => {
            model.expanded.insert(id);
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
            expire_outbox(model);
            let mut changed = model.composer.outbox.len() != pending;
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
            save
        }
        Event::AppFocus { focused } => {
            model.focused = focused;
            Command::done()
        }
        Event::DismissToast => {
            model.toast = model.toast_queue.pop_front();
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
            let path = path.trim_end_matches('/').to_owned();
            if path.is_empty() {
                return Command::done();
            }
            let title = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let create = ProjectChange::Create { project: &ProjectId::from(uuid()), title: &title, workspace_root: &path };
            call(model, requests::projects_mutate(&uuid(), create), Route::ProjectCreated)
        }
        Event::CreateProject { name } => {
            if name.trim().is_empty() {
                return Command::done();
            }
            call(model, requests::projects_create_new(name.trim()), Route::ProjectCreated)
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
            // A trailing slash lists the folder's children.
            let path = match path.trim() {
                "" => "~".to_owned(),
                "/" => "/".to_owned(),
                p => p.trim_end_matches('/').to_owned(),
            };
            b.path = path.clone();
            let rev = b.revision;
            let partial = if path == "/" { path } else { format!("{path}/") };
            call(model, requests::filesystem_browse(&partial), Route::Browse(rev))
        }

        // ---- thread ----
        Event::Respond { request_id, decision } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            dispatch(model, respond(&tid, &request_id, &decision, None), "Respond")
        }
        Event::RespondUserInput { request_id, answers } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            let answers = answers.into_iter().filter(|a| !a.values.is_empty()).map(|a| (a.question_id, a.values)).collect();
            dispatch(model, respond(&tid, &request_id, "accept", Some(answers)), "Answer")
        }
        Event::DismissUserInput { request_id } => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
            dispatch(model, ThreadUserInputDismiss { command_id: command_id(), thread_id: tid.into(), request_id: request_id.into() }, "Skip question")
        }
        Event::Interrupt => {
            let Some(tid) = model.selected_thread.clone() else { return Command::done() };
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
            let opt = model_options(model).into_iter().map(|(_, o)| o).find(|o| o.instance_id == instance_id && o.model == slug);
            let Some(opt) = opt else { return Command::done() };
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
            set_draft(model, format!("/{} ", name.trim_start_matches('/')))
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
        Event::SelectTerminal { terminal_id } => {
            model.panel.active_terminal = Some(terminal_id);
            render()
        }
        Event::CloseTerminal { terminal_id } => {
            let Some(thread) = model.panel.terminal_owner.clone() else { return Command::done() };
            let route = Route::Terminal { thread_id: thread.clone(), terminal_id: terminal_id.clone() };
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
            let thread = ThreadId::from(thread);
            let clear = call(model, requests::terminal_clear(&thread, &id), Route::Command("Clear terminal".into()));
            // Ctrl-L: the shell draws its prompt again on the empty screen.
            let redraw = call(model, requests::terminal_write(&thread, &id, "\u{c}"), Route::Command("Terminal".into()));
            clear.and(redraw)
        }
        Event::TerminalPaste { text } => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            if let Some(t) = model.panel.terminals.iter_mut().find(|t| t.id == id) { t.scroll_to_bottom(); }
            let bracketed = model.panel.terminals.iter().find(|t| t.id == id).is_some_and(|t| t.screen().bracketed_paste());
            let data = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            call(model, requests::terminal_write(&thread.into(), &id, &data), Route::Command("Terminal".into()))
        }
        Event::TerminalResize { rows, cols } => {
            let (Some(thread), Some(id)) = (model.panel.terminal_owner.clone(), model.panel.active_terminal.clone()) else { return Command::done() };
            let (rows, cols) = (rows.clamp(2, 500) as u16, cols.clamp(10, 1000) as u16);
            let Some(t) = model.panel.terminals.iter_mut().find(|t| t.id == id) else { return Command::done() };
            if t.size() == (rows, cols) {
                return Command::done();
            }
            t.resize(rows, cols);
            call(model, requests::terminal_resize(&thread.into(), &id, cols, rows), Route::Command("Terminal".into())).and(render())
        }
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
        Event::OpenFile { path } => {
            let cwd = model.panel.files.root.clone();
            model.panel.files.preview = Some(crate::model::Preview { path: path.clone(), loading: true, ..Default::default() });
            call(model, requests::projects_read_file(&cwd, &path), Route::ReadFile(path)).and(render())
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
            "providers" | "general" => call(model, requests::server_refresh_providers(false), Route::Command("Refresh providers".into())),
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
        Event::RunStorageCleanup => {
            model.pages.storage_busy = true;
            call(model, requests::server_run_storage_cleanup(), Route::Command("Storage cleanup".into())).and(render())
        }
        Event::ReclaimDatabase => {
            model.pages.storage_busy = true;
            call(model, requests::server_reclaim_database_space(), Route::Command("Reclaim space".into())).and(render())
        }
        Event::SetScheduledEnabled { id, enabled } => call(model, requests::scheduled_tasks_set_enabled(&id.into(), enabled), Route::Command("Scheduled task".into())),
        Event::RunScheduledNow { id } => call(model, requests::scheduled_tasks_run_now(&id.into()), Route::Command("Run task".into())),
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
            let Some(e) = model.pages.prs.iter().find(|p| crate::present::pr_key(p) == key) else { return render() };
            let (summary, checks) = (requests::pull_requests_summary(e), requests::pull_requests_checks(e));
            let a = call(model, summary, Route::PullRequestDetail(key.clone()));
            let b = call(model, checks, Route::PullRequestChecks(key));
            a.and(b).and(render())
        }
        Event::LoadUsage { days } => {
            model.pages.usage_days = days.max(1);
            model.pages.usage_loading = true;
            let until = utc_day(model.now_ms);
            let since = utc_day(model.now_ms - (days.max(1) as i64 - 1) * 86_400_000);
            call(model, requests::server_get_usage_summary(&since, &until), Route::Usage).and(render())
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
