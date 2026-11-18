//! Everything the core asks of a server: one function per RPC method, and
//! one per JSON body it posts over HTTP.
//!
//! This is the edge where what the core means becomes JSON. A function takes
//! what its request is about as ids, enums and records, and builds the
//! payload here; its callers never write JSON. The two orchestration methods
//! carry records of their own, a [`Command`] and a [`ThreadLaunchInput`],
//! which go out as the server decodes them. A [`Request`] is written once,
//! into the frame that carries it (`RpcClient::request`).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "what the core asks of a server becomes JSON here: one function per RPC method and HTTP body")]

use crate::model::Attachment;
use crate::settings::Typed;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};
use domovoi_model::typed::{
    Command, CreationSource, GitStackedAction, ImageAttachment, InteractionMode, MessageId, ModelSelection, ProjectId, ProviderInstanceId,
    PullRequestAction, PullRequestListEntry, PullRequestMergeMethod, RuntimeMode, ScheduledTaskId, ScheduledTaskSchedule, ServerSettings,
    ThreadFindMatch, ThreadId, ThreadLaunchInput, WorkspaceStrategy,
};

/// A request of one RPC method, ready to be written.
#[derive(Debug, Clone)]
pub struct Request {
    method: &'static str,
    payload: Payload,
}

/// What a request carries.
#[derive(Debug, Clone)]
enum Payload {
    Command(Command),
    Launch(Box<ThreadLaunchInput>),
    /// A payload one of this module's functions built.
    Built(Value),
}

impl Serialize for Payload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Command(command) => command.serialize(serializer),
            Self::Launch(launch) => launch.serialize(serializer),
            Self::Built(payload) => payload.serialize(serializer),
        }
    }
}

impl Request {
    /// The RPC method, such as `orchestration.dispatchCommand`.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// What the method is called with, to write into the request's frame.
    pub fn payload(&self) -> &impl Serialize {
        &self.payload
    }
}

fn built(method: &'static str, payload: Value) -> Request {
    Request { method, payload: Payload::Built(payload) }
}

/// A method called with nothing.
fn bare(method: &'static str) -> Request {
    built(method, json!({}))
}

// ---- orchestration ----

pub fn orchestration_dispatch_command(command: Command) -> Request {
    Request { method: "orchestration.dispatchCommand", payload: Payload::Command(command) }
}

pub fn orchestration_launch_thread(launch: ThreadLaunchInput) -> Request {
    Request { method: "orchestration.launchThread", payload: Payload::Launch(Box::new(launch)) }
}

/// The answers to a question's parts, as `runtime-request.respond` carries
/// them: each question's id with what was chosen or typed for it. (The
/// schema leaves an answer's shape open; the core's are lists of text.)
pub fn answers(given: impl IntoIterator<Item = (String, Vec<String>)>) -> Map<String, Value> {
    given.into_iter().map(|(question, values)| (question, json!(values))).collect()
}

/// The thread list's stream, from after `after_sequence` when some of it is held.
pub fn orchestration_subscribe_shell(after_sequence: Option<u64>) -> Request {
    let mut payload = json!({ "requestCompletionMarker": true });
    if let Some(after) = after_sequence {
        payload["afterSequence"] = json!(after);
    }
    built("orchestration.subscribeShell", payload)
}

/// A thread's stream, from after `after_sequence` when the thread is held.
pub fn orchestration_subscribe_thread(thread: &ThreadId, after_sequence: Option<u64>) -> Request {
    let mut payload = json!({ "threadId": thread, "requestCompletionMarker": true });
    if let Some(after) = after_sequence {
        payload["afterSequence"] = json!(after);
    }
    built("orchestration.subscribeThread", payload)
}

pub fn orchestration_get_archived_shell_snapshot() -> Request {
    bare("orchestration.getArchivedShellSnapshot")
}

/// Everything a thread changed, up to its turn `to_turn_count`.
pub fn orchestration_get_full_thread_diff(thread: &ThreadId, to_turn_count: u32) -> Request {
    built("orchestration.getFullThreadDiff", json!({ "threadId": thread, "toTurnCount": to_turn_count }))
}

/// What a thread's turn `turn` changed.
pub fn orchestration_get_turn_diff(thread: &ThreadId, turn: u32) -> Request {
    built("orchestration.getTurnDiff", json!({ "threadId": thread, "fromTurnCount": turn.saturating_sub(1), "toTurnCount": turn }))
}

pub fn orchestration_search_threads(query: &str, limit: u32) -> Request {
    built("orchestration.searchThreads", json!({ "query": query, "limit": limit }))
}

/// A step from one match of a search inside a thread to the next or the one before.
pub struct FindStep<'a> {
    pub forward: bool,
    /// The match to step from; the first or last match when there is none.
    pub from: Option<&'a ThreadFindMatch>,
}

/// A search inside a thread, or a step among its matches.
pub fn orchestration_search_thread(thread: &ThreadId, query: &str, step: Option<FindStep>) -> Request {
    let mut payload = json!({ "threadId": thread, "query": query });
    if let Some(step) = step {
        payload["offset"] = json!(if step.forward { 1 } else { -1 });
        if let Some(from) = step.from {
            payload["start"] = json!({ "entryId": from.entry_id, "occurrence": from.occurrence });
        }
    }
    built("orchestration.searchThread", payload)
}

// ---- the server ----

pub fn subscribe_server_config() -> Request {
    built("subscribeServerConfig", json!({ "environmentThemes": false, "usageLimitSources": false }))
}

pub fn subscribe_server_lifecycle() -> Request {
    bare("subscribeServerLifecycle")
}

pub fn subscribe_auth_access() -> Request {
    bare("subscribeAuthAccess")
}

/// What `server.updateSettings` changes.
pub enum SettingsPatch<'a> {
    /// Settings as their record holds them: the ones set are the ones changed.
    Settings(&'a ServerSettings),
    /// A value its setting's record cannot hold (words where a number goes,
    /// a setting this build does not know), sent as it was typed for the
    /// server to judge.
    AsTyped { key: &'a str, value: &'a Typed },
    /// A model starred or not, hidden or shown.
    Model { provider: &'a ProviderInstanceId, model: &'a str, favorite: Option<bool>, hidden: Option<bool> },
}

pub fn server_update_settings(patch: SettingsPatch) -> Request {
    let patch = match patch {
        SettingsPatch::Settings(settings) => json!(settings),
        SettingsPatch::AsTyped { key, value } => {
            let value = match value {
                Typed::Switch(on) => json!(on),
                Typed::Text(text) => json!(text),
                Typed::Number(number) => json!(number),
            };
            json!({ key: value })
        }
        SettingsPatch::Model { provider, model, favorite, hidden } => {
            let mut patch = json!({});
            if let Some(favorite) = favorite {
                patch["setModelFavorites"] = json!([{ "provider": provider, "model": model, "favorite": favorite }]);
            }
            if let Some(hidden) = hidden {
                patch["setModelsHidden"] = json!([{ "provider": provider, "model": model, "hidden": hidden }]);
            }
            patch
        }
    };
    built("server.updateSettings", json!({ "patch": patch }))
}

/// The providers' statuses again; `fresh` has the server probe them anew.
pub fn server_refresh_providers(fresh: bool) -> Request {
    built("server.refreshProviders", if fresh { json!({ "fresh": true }) } else { json!({}) })
}

pub fn server_remove_keybinding(shortcut: &str, command: &str, when: Option<&str>) -> Request {
    let mut payload = json!({ "key": shortcut, "command": command });
    if let Some(when) = when {
        payload["when"] = json!(when);
    }
    built("server.removeKeybinding", payload)
}

pub fn server_get_database_storage() -> Request {
    bare("server.getDatabaseStorage")
}

pub fn server_get_storage_cleanup_report() -> Request {
    bare("server.getStorageCleanupReport")
}

pub fn server_run_storage_cleanup() -> Request {
    bare("server.runStorageCleanup")
}

pub fn server_reclaim_database_space() -> Request {
    bare("server.reclaimDatabaseSpace")
}

pub fn server_get_process_diagnostics() -> Request {
    bare("server.getProcessDiagnostics")
}

pub fn server_get_host_resources() -> Request {
    bare("server.getHostResources")
}

/// Usage by day, from `since_day` to `until_day` (`YYYY-MM-DD`, UTC).
pub fn server_get_usage_summary(since_day: &str, until_day: &str) -> Request {
    built("server.getUsageSummary", json!({ "sinceDay": since_day, "untilDay": until_day, "timeZone": "UTC" }))
}

// ---- projects and folders ----

/// A change to the server's projects (`projects.mutate`).
pub enum ProjectChange<'a> {
    /// A project for a folder that is there already.
    Create { project: &'a ProjectId, title: &'a str, workspace_root: &'a str },
    Rename { project: &'a ProjectId, title: &'a str },
    Delete { project: &'a ProjectId },
}

/// `command_id` names the change, as a command id does a command.
pub fn projects_mutate(command_id: &str, change: ProjectChange) -> Request {
    let payload = match change {
        ProjectChange::Create { project, title, workspace_root } => json!({
            "type": "project.create", "commandId": command_id, "projectId": project, "title": title,
            "workspaceRoot": workspace_root, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null,
        }),
        ProjectChange::Rename { project, title } => json!({ "type": "project.update", "commandId": command_id, "projectId": project, "title": title }),
        ProjectChange::Delete { project } => json!({ "type": "project.delete", "commandId": command_id, "projectId": project }),
    };
    built("projects.mutate", payload)
}

/// A new project in a new folder named `name`.
pub fn projects_create_new(name: &str) -> Request {
    built("projects.createNew", json!({ "name": name }))
}

pub fn projects_ensure_scratch() -> Request {
    bare("projects.ensureScratch")
}

pub fn projects_create_from_thread(thread: &ThreadId) -> Request {
    built("projects.createFromThread", json!({ "threadId": thread }))
}

pub fn projects_list_entries(cwd: &str, directory: &str) -> Request {
    built("projects.listEntries", json!({ "cwd": cwd, "directoryPath": directory }))
}

pub fn projects_search_entries(cwd: &str, query: &str, limit: u32) -> Request {
    built("projects.searchEntries", json!({ "cwd": cwd, "query": query, "limit": limit }))
}

pub fn projects_read_file(cwd: &str, relative_path: &str) -> Request {
    built("projects.readFile", json!({ "cwd": cwd, "relativePath": relative_path }))
}

pub fn filesystem_browse(partial_path: &str) -> Request {
    built("filesystem.browse", json!({ "partialPath": partial_path }))
}

pub fn agent_sessions_scan() -> Request {
    bare("agentSessions.scan")
}

/// The agents' own sessions of the folder `workspace_root`, into `project`.
pub fn agent_sessions_import(project: &ProjectId, workspace_root: &str) -> Request {
    built("agentSessions.import", json!({ "projectId": project, "expectedWorkspaceRoot": workspace_root }))
}

// ---- attachments ----

/// A URL to show an attached image from.
pub fn assets_create_url(image: &ImageAttachment) -> Request {
    let resource = json!({ "_tag": "attachment", "attachmentId": image.id, "fileName": image.name, "mimeType": image.mime_type, "disposition": "inline" });
    built("assets.createUrl", json!({ "resource": resource }))
}

/// The images picked for the message `message`, for the server to keep.
pub fn assets_persist_chat_attachments(thread: &ThreadId, message: &MessageId, images: &[&Attachment]) -> Request {
    let attachments: Vec<Value> =
        images.iter().map(|a| json!({ "type": "image", "name": a.name, "mimeType": a.mime_type, "sizeBytes": a.size_bytes, "dataUrl": a.data_url })).collect();
    built("assets.persistChatAttachments", json!({ "threadId": thread, "messageId": message, "attachments": attachments }))
}

// ---- diffs, git and pull requests ----

/// What a diff preview of a folder shows.
pub enum DiffSource {
    /// The branch against the one it was cut from.
    BranchRange,
    /// What is not committed.
    WorkingTree,
}

pub fn review_get_diff_preview(cwd: &str, source: DiffSource) -> Request {
    let kind = match source {
        DiffSource::BranchRange => "branch-range",
        DiffSource::WorkingTree => "working-tree",
    };
    built("review.getDiffPreview", json!({ "cwd": cwd, "sourceKind": kind }))
}

pub fn subscribe_vcs_status(cwd: &str) -> Request {
    built("subscribeVcsStatus", json!({ "cwd": cwd, "includeRemote": true }))
}

pub fn vcs_refresh_status(cwd: &str) -> Request {
    built("vcs.refreshStatus", json!({ "cwd": cwd }))
}

/// The local branches of the folder `cwd`.
pub fn vcs_list_refs(cwd: &str) -> Request {
    built("vcs.listRefs", json!({ "cwd": cwd, "limit": 100, "refKind": "local" }))
}

pub fn vcs_pull(cwd: &str) -> Request {
    built("vcs.pull", json!({ "cwd": cwd }))
}

pub fn vcs_switch_ref(cwd: &str, name: &str) -> Request {
    built("vcs.switchRef", json!({ "cwd": cwd, "refName": name }))
}

/// A new branch, switched to.
pub fn vcs_create_ref(cwd: &str, name: &str) -> Request {
    built("vcs.createRef", json!({ "cwd": cwd, "refName": name, "switchRef": true }))
}

/// A commit, push or pull request from the folder `cwd`; `action_id` names
/// the action in its progress stream.
pub fn git_run_stacked_action(action_id: &str, cwd: &str, action: &GitStackedAction, commit_message: Option<&str>, thread: Option<&ThreadId>) -> Request {
    let mut payload = json!({ "actionId": action_id, "cwd": cwd, "action": action });
    if let Some(message) = commit_message {
        payload["commitMessage"] = json!(message);
    }
    if let Some(thread) = thread {
        payload["threadId"] = json!(thread);
    }
    built("git.runStackedAction", payload)
}

/// The pull request `reference` checked out in a worktree of its own.
pub fn git_prepare_pull_request_thread(cwd: &str, reference: &str) -> Request {
    built("git.preparePullRequestThread", json!({ "cwd": cwd, "reference": reference, "mode": "worktree" }))
}

/// The open pull requests of the server's projects.
pub fn pull_requests_list() -> Request {
    built("pullRequests.list", json!({ "state": "open", "limit": 50 }))
}

/// What names a pull request to the server, with `more` about the call.
fn about(pull_request: &PullRequestListEntry, mut more: Value) -> Value {
    more["projectId"] = json!(pull_request.project_id);
    more["repository"] = json!(pull_request.repository);
    more["number"] = json!(pull_request.number);
    more
}

pub fn pull_requests_summary(pull_request: &PullRequestListEntry) -> Request {
    built("pullRequests.summary", about(pull_request, json!({})))
}

pub fn pull_requests_checks(pull_request: &PullRequestListEntry) -> Request {
    built("pullRequests.checks", about(pull_request, json!({})))
}

/// `merge_method` is how a merge lands, and is sent with a merge only.
pub fn pull_requests_run_action(pull_request: &PullRequestListEntry, action: &PullRequestAction, merge_method: Option<&PullRequestMergeMethod>) -> Request {
    let mut more = json!({ "action": action });
    if let Some(method) = merge_method {
        more["mergeMethod"] = json!(method);
    }
    built("pullRequests.runAction", about(pull_request, more))
}

/// A review with no comments on lines; `verdict` is the reviewer's, such as `approve`.
pub fn pull_requests_submit_review(pull_request: &PullRequestListEntry, verdict: &str, body: &str) -> Request {
    built("pullRequests.submitReview", about(pull_request, json!({ "verdict": verdict, "body": body, "comments": [] })))
}

pub fn pull_requests_comment(pull_request: &PullRequestListEntry, body: &str) -> Request {
    built("pullRequests.comment", about(pull_request, json!({ "body": body })))
}

// ---- terminals ----

pub fn subscribe_terminal_metadata() -> Request {
    bare("subscribeTerminalMetadata")
}

/// The terminal `terminal` of a thread, started in `cwd` if it is not running.
pub fn terminal_attach(thread: &ThreadId, terminal: &str, cwd: &str, cols: u16, rows: u16) -> Request {
    built("terminal.attach", json!({ "threadId": thread, "terminalId": terminal, "cwd": cwd, "cols": cols, "rows": rows }))
}

pub fn terminal_write(thread: &ThreadId, terminal: &str, data: &str) -> Request {
    built("terminal.write", json!({ "threadId": thread, "terminalId": terminal, "data": data }))
}

pub fn terminal_clear(thread: &ThreadId, terminal: &str) -> Request {
    built("terminal.clear", json!({ "threadId": thread, "terminalId": terminal }))
}

pub fn terminal_resize(thread: &ThreadId, terminal: &str, cols: u16, rows: u16) -> Request {
    built("terminal.resize", json!({ "threadId": thread, "terminalId": terminal, "cols": cols, "rows": rows }))
}

pub fn terminal_close(thread: &ThreadId, terminal: &str) -> Request {
    built("terminal.close", json!({ "threadId": thread, "terminalId": terminal }))
}

// ---- scheduled tasks ----

pub fn scheduled_tasks_subscribe() -> Request {
    bare("scheduledTasks.subscribe")
}

pub fn scheduled_tasks_set_enabled(task: &ScheduledTaskId, enabled: bool) -> Request {
    built("scheduledTasks.setEnabled", json!({ "id": task, "enabled": enabled }))
}

pub fn scheduled_tasks_run_now(task: &ScheduledTaskId) -> Request {
    built("scheduledTasks.runNow", json!({ "id": task }))
}

pub fn scheduled_tasks_delete(task: &ScheduledTaskId) -> Request {
    built("scheduledTasks.delete", json!({ "id": task }))
}

/// A task the user sets up: `prompt` sent on `schedule`, each time to a new
/// thread in the project's folder.
pub struct NewScheduledTask<'a> {
    pub title: &'a str,
    pub prompt: &'a str,
    pub schedule: &'a ScheduledTaskSchedule,
    pub project: &'a ProjectId,
    pub model_selection: &'a ModelSelection,
    pub runtime_mode: &'a RuntimeMode,
}

pub fn scheduled_tasks_upsert(task: NewScheduledTask) -> Request {
    built("scheduledTasks.upsert", json!({
        "title": task.title, "prompt": task.prompt, "enabled": true, "schedule": task.schedule,
        "projectId": task.project, "threadMode": "new_thread", "workspaceStrategy": WorkspaceStrategy::Root { branch: None },
        "modelSelection": task.model_selection, "runtimeMode": task.runtime_mode, "interactionMode": InteractionMode::Default,
        "creationSource": CreationSource::Web,
    }))
}

// ---- over HTTP ----

/// The JSON body of a request the core posts over HTTP.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct Body(Value);

/// `POST /api/domovoi/push/devices`: where this device's notifications go.
pub fn push_device(token: &str, environment: &str) -> Body {
    Body(json!({ "token": token, "environment": environment }))
}

/// `POST /api/domovoi/push/activities`: where a thread's Live Activity is updated.
pub fn push_activity(token: &str, thread: &ThreadId, environment: &str) -> Body {
    Body(json!({ "token": token, "threadId": thread, "environment": environment }))
}

/// `POST /api/domovoi/remote/configure`: remote access on or off.
pub fn remote_configure(enabled: bool) -> Body {
    Body(json!({ "enabled": enabled }))
}

/// `POST /api/domovoi/remote/pairing`: a pairing link good for `ttl_minutes`.
pub fn remote_pairing(ttl_minutes: u32) -> Body {
    Body(json!({ "ttlMinutes": ttl_minutes }))
}

/// `POST /api/auth/clients/revoke`: signs a device out.
pub fn auth_clients_revoke(session_id: &str) -> Body {
    Body(json!({ "sessionId": session_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(request: &Request) -> Value {
        json!({ "method": request.method(), "payload": request.payload() })
    }

    /// A settings record with nothing set changes nothing, and one with a
    /// setting set is a patch of that setting alone.
    #[test]
    fn a_settings_record_is_a_patch_of_what_it_sets() {
        let nothing = ServerSettings::default();
        assert_eq!(sent(&server_update_settings(SettingsPatch::Settings(&nothing))), json!({ "method": "server.updateSettings", "payload": { "patch": {} } }));
        let one = ServerSettings { default_auto_pull: Some(true), ..ServerSettings::default() };
        assert_eq!(sent(&server_update_settings(SettingsPatch::Settings(&one)))["payload"], json!({ "patch": { "defaultAutoPull": true } }));
    }

    #[test]
    fn a_command_goes_out_as_the_record_it_is() {
        let command = Command::from_value(json!({ "type": "thread.unsettle", "commandId": "c1", "threadId": "t1", "reason": "user" })).expect("a command");
        let request = orchestration_dispatch_command(command);
        assert_eq!(sent(&request), json!({ "method": "orchestration.dispatchCommand", "payload": { "type": "thread.unsettle", "commandId": "c1", "threadId": "t1", "reason": "user" } }));
    }
}
