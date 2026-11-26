//! Why a command or a launch was not carried out, and how that is told to
//! the client: as t3code's `OrchestrationV2DispatchCommandError` and
//! `OrchestrationV2ThreadLaunchError`.

use crate::orchestration::context::NoForkSource;
use crate::store::StoreError;
use serde::Serialize;
use serde::ser::SerializeMap;
use domovoi_model::typed::{CommandId, NodeId, PlanId, ProjectId, RunId, ThreadId, TurnItemId};

/// Why a handler refused a command, or could not carry it out. Its text is
/// the `message` the client gets.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("commandId required")]
    NoCommandId,
    /// A command that names no type is refused as one of an unknown type is, whose name is then empty.
    #[error("Command  is not supported by domovoi-server yet.")]
    NoCommandType,
    #[error("Command {command_type} is not one domovoi-server can read: {reason}")]
    Malformed { command_type: String, reason: String },
    #[error("Command {0} is not supported by domovoi-server yet.")]
    Unsupported(String),
    #[error("Unknown thread {0}")]
    UnknownThread(ThreadId),
    #[error("The thread was deleted.")]
    ThreadDeleted,
    /// The store failed, or refused a record.
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("Thread {0} already exists.")]
    ThreadExists(ThreadId),

    // Messages.
    #[error("Proposed plan {plan} does not exist on thread {thread}.")]
    NoSuchPlan { plan: PlanId, thread: ThreadId },
    #[error("Proposed plan {0} is not active.")]
    PlanNotActive(PlanId),
    #[error("The run this fork continues from is gone.")]
    ForkSourceGone,

    // The queue.
    #[error("Unknown run {0}.")]
    UnknownRun(RunId),
    #[error("Run {0} is not queued.")]
    RunNotQueued(RunId),
    #[error("The queued run has no message.")]
    QueuedRunWithoutMessage,
    #[error("The target run is not running a turn that can take more input.")]
    TargetNotSteerable,

    // The thread's own fields.
    #[error("A thread tag must be one line of at most 64 characters.")]
    InvalidTag,
    #[error("snoozedUntil must be a date and time.")]
    SnoozeNotATime,
    #[error("That run is not the thread's latest, stopped by a usage limit that resets then.")]
    NotUsageLimited,
    #[error("Thread {thread} cannot move to missing project {}.", quoted(.project.as_str()))]
    AdoptMissingProject { thread: ThreadId, project: ProjectId },
    #[error("Thread {thread} does not work in {root}.")]
    AdoptOtherFolder { thread: ThreadId, root: String },
    #[error("Thread {0} is settled or archived and cannot watch pull requests.")]
    WatchOnSettledThread(ThreadId),
    #[error("Thread {0} is a subagent; its parent thread watches pull requests.")]
    WatchFromSubagent(ThreadId),

    // Runs, requests and checkpoints.
    #[error("Unknown subagent.")]
    UnknownSubagent,
    #[error("The subagent is not running.")]
    SubagentNotRunning,
    #[error("The provider gave this subagent no id to stop it by.")]
    SubagentWithoutNativeId,
    #[error("Unknown runtime request.")]
    UnknownRuntimeRequest,
    #[error("The request is no longer pending.")]
    RequestNotPending,
    #[error("Answer the required questions using their offered options or permitted custom text.")]
    InvalidQuestionAnswer,
    #[error("The thread has no active run.")]
    NoActiveRun,
    #[error("Node {node} is not the root of run {run}.")]
    NotRunRoot { node: NodeId, run: RunId },
    #[error("Turn item {0} is not a secret request.")]
    NotASecretRequest(TurnItemId),
    #[error("Secret request {0} belongs to another run.")]
    SecretRequestOfAnotherRun(TurnItemId),
    #[error("Unknown checkpoint.")]
    UnknownCheckpoint,
    #[error("Stop the active run before reverting.")]
    RunStillActive,
    #[error(
        "File restore requires an isolated worktree. This workspace may contain changes from another thread or a side chat. Rewind the conversation without restoring files instead, or delete side chats you no longer need."
    )]
    RestoreNeedsWorktree,

    // Forks.
    #[error(transparent)]
    NoForkSource(#[from] NoForkSource),
    #[error("Only a fork can be merged back, and only into the thread it came from.")]
    NotAForkOfTarget,
}

impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        Refusal::Store(StoreError::Db(error))
    }
}

impl Refusal {
    /// The refusals of `thread.project.adopt-folder`, which the client is
    /// told as the older `OrchestrationDispatchCommandError`: a message and
    /// nothing else.
    fn is_untagged_by_command(&self) -> bool {
        matches!(self, Refusal::AdoptMissingProject { .. } | Refusal::AdoptOtherFolder { .. })
    }
}

/// A command that was not carried out: which one, and why.
#[derive(Debug, thiserror::Error)]
#[error("{refusal}")]
pub struct DispatchFailure {
    /// `None` only for a command that named no id.
    pub command_id: Option<CommandId>,
    /// `None` only for a command that named no type.
    pub command_type: Option<String>,
    #[source]
    pub refusal: Refusal,
}

impl Serialize for DispatchFailure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut error = serializer.serialize_map(None)?;
        if self.refusal.is_untagged_by_command() {
            error.serialize_entry("_tag", "OrchestrationDispatchCommandError")?;
        } else {
            error.serialize_entry("_tag", "OrchestrationV2DispatchCommandError")?;
            error.serialize_entry("commandId", self.command_id.as_ref().map_or("unknown", CommandId::as_str))?;
            error.serialize_entry("commandType", self.command_type.as_deref().unwrap_or("unknown"))?;
        }
        error.serialize_entry("message", &self.refusal.to_string())?;
        error.end()
    }
}

/// Why a thread was not launched. Its text is the `message` the client gets.
#[derive(Debug, thiserror::Error)]
pub enum LaunchRefusal {
    #[error("commandId required")]
    NoCommandId,
    #[error("The launch is not one domovoi-server can read: {0}")]
    Malformed(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("Thread {0} already exists.")]
    ThreadExists(ThreadId),
    #[error("Unknown project.")]
    UnknownProject,
    #[error("Worktree {0} does not exist.")]
    NoSuchWorktree(String),
    /// The worktree could not be made; the text is git's, or the setup tracker's.
    #[error("{0}")]
    Worktree(String),
    /// The thread's own folder (in the scratch project) could not be made.
    #[error(transparent)]
    Folder(#[from] std::io::Error),
    /// The thread's first message was refused.
    #[error(transparent)]
    InitialMessage(Refusal),
    /// The issue the thread was to be started on could not be read; the text
    /// is the tracker's.
    #[error("The issue could not be read, so the thread was not started. {0}")]
    Issue(String),
    #[error("thread vanished")]
    ThreadVanished,
}

impl From<rusqlite::Error> for LaunchRefusal {
    fn from(error: rusqlite::Error) -> Self {
        LaunchRefusal::Store(StoreError::Db(error))
    }
}

/// A launch that was not carried out: which one, and why.
#[derive(Debug, thiserror::Error)]
#[error("{refusal}")]
pub struct LaunchFailure {
    /// `None` only for a launch that named no id.
    pub command_id: Option<CommandId>,
    /// `None` only for a launch that named no project.
    pub project_id: Option<ProjectId>,
    #[source]
    pub refusal: LaunchRefusal,
}

impl Serialize for LaunchFailure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut error = serializer.serialize_map(None)?;
        error.serialize_entry("_tag", "OrchestrationV2ThreadLaunchError")?;
        error.serialize_entry("commandId", &self.command_id)?;
        error.serialize_entry("projectId", &self.project_id)?;
        error.serialize_entry("message", &self.refusal.to_string())?;
        error.end()
    }
}

/// `text` as JSON writes a string: quoted, with what needs it escaped.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_are_written_as_tagged_errors() {
        let failure = DispatchFailure { command_id: Some("c1".into()), command_type: Some("thread.visit".into()), refusal: Refusal::UnknownThread("t9".into()) };
        assert_eq!(
            json!(failure),
            json!({ "_tag": "OrchestrationV2DispatchCommandError", "commandId": "c1", "commandType": "thread.visit", "message": "Unknown thread t9" })
        );
        let failure = DispatchFailure { command_id: None, command_type: None, refusal: Refusal::NoCommandId };
        assert_eq!(
            json!(failure),
            json!({ "_tag": "OrchestrationV2DispatchCommandError", "commandId": "unknown", "commandType": "unknown", "message": "commandId required" })
        );
        let refusal = Refusal::AdoptMissingProject { thread: "t1".into(), project: "p \"2\"".into() };
        let failure = DispatchFailure { command_id: Some("c1".into()), command_type: Some("thread.project.adopt-folder".into()), refusal };
        assert_eq!(
            json!(failure),
            json!({ "_tag": "OrchestrationDispatchCommandError", "message": "Thread t1 cannot move to missing project \"p \\\"2\\\"\"." })
        );
        let failure = LaunchFailure { command_id: Some("c1".into()), project_id: Some("p".into()), refusal: LaunchRefusal::UnknownProject };
        assert_eq!(json!(failure), json!({ "_tag": "OrchestrationV2ThreadLaunchError", "commandId": "c1", "projectId": "p", "message": "Unknown project." }));
        let failure = LaunchFailure { command_id: None, project_id: None, refusal: LaunchRefusal::NoCommandId };
        assert_eq!(json!(failure), json!({ "_tag": "OrchestrationV2ThreadLaunchError", "commandId": null, "projectId": null, "message": "commandId required" }));
    }
}
