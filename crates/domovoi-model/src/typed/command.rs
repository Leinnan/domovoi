//! Commands: what a client, or the server itself, asks the orchestrator to do.
//!
//! [`Command`] has one variant per `type` of t3code's `OrchestrationV2Command`
//! and of its server-only `OrchestrationV2InternalCommand`, each with the
//! fields its schema defines. A command of a type this build does not know is
//! [`Command::Other`], which keeps the command whole. A command of a known
//! type must decode in full: one with a field missing or of the wrong type is
//! an error, never an `Other`.
//!
//! Beside the commands are what `orchestration.launchThread` takes and
//! answers ([`ThreadLaunchInput`], [`ThreadLaunchResult`]), what
//! `orchestration.dispatchCommand` answers ([`DispatchCommandResult`]), and
//! the errors both fail with.
//!
//! Five fields t3code requires are optional here, each marked where it is
//! declared: domovoi-server never reads them and has always carried out a command
//! sent without them.

use super::attachment::ChatAttachment;
use super::common::{
    Actor, ApprovalDecision, CreationSource, IdleCompaction, InteractionMode, LinkedThread, McpClientAccess, ModelSelection,
    ProviderRef, RuntimeMode, Timestamp, double_option,
};
use super::ids::{
    CheckpointId, CheckpointScopeId, CommandId, EnvironmentId, GuestMessageId, GuestShareId, MessageId, NodeId,
    ProjectId, ProviderDriverKind, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunId, RuntimeRequestId,
    ScheduledTaskId, ThreadId, TurnItemId,
};
use super::macros::{string_enum, tagged_union_deserialize};
use super::message::{DelegatedCompletionReport, MessageContext, Notification, PullRequestWakeSection};
use super::provider::ProviderThreadNativeMetadata;
use super::pull_request::{LinkedPullRequest, PullRequestLinkSource, PullRequestSnapshot, PullRequestStack, PullRequestWatch};
use super::run::{CompletionWake, RemoteTaskChild, SourcePlanRef, WorkspaceStrategy};
use super::stream::ThreadProjection;
use super::thread::{LinkOrigin, TitleRegenerationStatus};
use super::turn_item::{LinkRequestStatus, ProviderFailure, SecretDelivery, SecretRequestStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Declares a command's record. `field: T` is required, `field?: T` may be
/// left out, and `field??: T` may be left out or `null`, the two kept apart.
macro_rules! command {
    ($(#[$meta:meta])* $name:ident { $($body:tt)* }) => {
        command!(@fields [$(#[$meta])*] $name [] $($body)*);
    };
    (@fields [$($meta:tt)*] $name:ident [$($done:tt)*]) => {
        $($meta)*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $($done)* }
    };
    (@fields $meta:tt $name:ident [$($done:tt)*] $(#[$field_meta:meta])* $field:ident ?? : $ty:ty, $($rest:tt)*) => {
        command!(@fields $meta $name [$($done)*
            $(#[$field_meta])*
            #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
            pub $field: Option<Option<$ty>>,
        ] $($rest)*);
    };
    (@fields $meta:tt $name:ident [$($done:tt)*] $(#[$field_meta:meta])* $field:ident ? : $ty:ty, $($rest:tt)*) => {
        command!(@fields $meta $name [$($done)*
            $(#[$field_meta])*
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub $field: Option<$ty>,
        ] $($rest)*);
    };
    (@fields $meta:tt $name:ident [$($done:tt)*] $(#[$field_meta:meta])* $field:ident : $ty:ty, $($rest:tt)*) => {
        command!(@fields $meta $name [$($done)* $(#[$field_meta])* pub $field: $ty,] $($rest)*);
    };
}

/// Defines [`Command`] from its wire types: the record each decodes as and the
/// field of that record naming the thread the command acts on.
macro_rules! commands {
    ($($tag:literal => $variant:ident($record:ident) on $thread:ident,)+) => {
        /// One request to change a thread (`OrchestrationV2ServerCommand`).
        #[derive(Clone, Debug, PartialEq, Serialize)]
        #[serde(tag = "type")]
        pub enum Command {
            $(#[serde(rename = $tag)] $variant(Box<$record>),)+
            /// A command of a type this build does not know, kept whole.
            #[serde(untagged)]
            Other(Box<OtherCommand>),
        }

        tagged_union_deserialize! {
            Command by "type" {
                $($tag => $variant,)+
                _ => Other,
            }
        }

        impl Command {
            /// Every command type this build knows.
            pub const TYPES: &'static [&'static str] = &[$($tag,)+];

            /// The wire name of the command's type, such as `message.dispatch`.
            pub fn type_name(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $tag,)+
                    Self::Other(other) => other.type_name(),
                }
            }

            /// The id the sender gave the command: a command sent twice with
            /// one id is carried out once.
            pub fn command_id(&self) -> &CommandId {
                match self {
                    $(Self::$variant(command) => &command.command_id,)+
                    Self::Other(other) => &other.command_id,
                }
            }

            /// The thread the command acts on (t3code's `commandThreadId`):
            /// for a fork or a merge back the thread that receives the
            /// context, for a delegated task its parent. `None` only for a
            /// command of an unknown type that names no `threadId`.
            pub fn thread_id(&self) -> Option<&ThreadId> {
                match self {
                    $(Self::$variant(command) => Some(&command.$thread),)+
                    Self::Other(other) => other.thread_id.as_ref(),
                }
            }
        }

        $(impl From<$record> for Command {
            fn from(command: $record) -> Self {
                Self::$variant(Box::new(command))
            }
        })+
    };
}

commands! {
    "thread.stop" => ThreadStop(ThreadStop) on thread_id,
    "thread.create" => ThreadCreate(ThreadCreate) on thread_id,
    "thread.archive" => ThreadArchive(ThreadArchive) on thread_id,
    "thread.guest-share.close" => ThreadGuestShareClose(ThreadGuestShareClose) on thread_id,
    "thread.guest-message.dismiss" => ThreadGuestMessageDismiss(ThreadGuestMessageDismiss) on thread_id,
    "thread.unarchive" => ThreadUnarchive(ThreadUnarchive) on thread_id,
    "thread.delete" => ThreadDelete(ThreadDelete) on thread_id,
    "thread.settle" => ThreadSettle(ThreadSettle) on thread_id,
    "thread.auto-settle" => ThreadAutoSettle(ThreadAutoSettle) on thread_id,
    "thread.unsettle" => ThreadUnsettle(ThreadUnsettle) on thread_id,
    "thread.snooze" => ThreadSnooze(ThreadSnooze) on thread_id,
    "thread.unsnooze" => ThreadUnsnooze(ThreadUnsnooze) on thread_id,
    "thread.auto-settle.set" => ThreadAutoSettleSet(ThreadAutoSettleSet) on thread_id,
    "thread.pin" => ThreadPin(ThreadPin) on thread_id,
    "thread.unpin" => ThreadUnpin(ThreadUnpin) on thread_id,
    "thread.tag.set" => ThreadTagSet(ThreadTagSet) on thread_id,
    "thread.pin.reorder" => ThreadPinReorder(ThreadPinReorder) on thread_id,
    "thread.active.reorder" => ThreadActiveReorder(ThreadActiveReorder) on thread_id,
    "thread.visit" => ThreadVisit(ThreadVisit) on thread_id,
    "thread.mark-unread" => ThreadMarkUnread(ThreadMarkUnread) on thread_id,
    "thread.metadata.update" => ThreadMetadataUpdate(ThreadMetadataUpdate) on thread_id,
    "thread.pull-request.link" => ThreadPullRequestLink(ThreadPullRequestLink) on thread_id,
    "thread.pull-request.unlink" => ThreadPullRequestUnlink(ThreadPullRequestUnlink) on thread_id,
    "thread.pull-request-link.sync" => ThreadPullRequestLinkSync(ThreadPullRequestLinkSync) on thread_id,
    "thread.pull-request.watch" => ThreadPullRequestWatch(ThreadPullRequestWatch) on thread_id,
    "thread.pull-request.sync" => ThreadPullRequestSync(ThreadPullRequestSync) on thread_id,
    "thread.title.regeneration.complete" => ThreadTitleRegenerationComplete(ThreadTitleRegenerationComplete) on thread_id,
    "thread.runtime-mode.set" => ThreadRuntimeModeSet(ThreadRuntimeModeSet) on thread_id,
    "thread.interaction-mode.set" => ThreadInteractionModeSet(ThreadInteractionModeSet) on thread_id,
    "thread.model-selection.set" => ThreadModelSelectionSet(ThreadModelSelectionSet) on thread_id,
    "provider-session.detach" => ProviderSessionDetach(ProviderSessionDetach) on thread_id,
    "message.dispatch" => MessageDispatch(MessageDispatch) on thread_id,
    "prepared-run.release" => PreparedRunRelease(PreparedRunRelease) on thread_id,
    "notification.delivery.accept" => NotificationDeliveryAccept(NotificationDeliveryAccept) on thread_id,
    "prepared-run.progress" => PreparedRunProgress(PreparedRunProgress) on thread_id,
    "prepared-run.fail" => PreparedRunFail(PreparedRunFail) on thread_id,
    "prepared-run.retry" => PreparedRunRetry(PreparedRunRetry) on thread_id,
    "subagent.stop" => SubagentStop(SubagentStop) on thread_id,
    "run.interrupt" => RunInterrupt(RunInterrupt) on thread_id,
    "queued-message.promote-to-steer" => QueuedMessagePromoteToSteer(QueuedMessagePromoteToSteer) on thread_id,
    "queue.resume" => QueueResume(QueueResume) on thread_id,
    "queued-run.start" => QueuedRunStart(QueuedRunStart) on thread_id,
    "queued-run.reorder" => QueuedRunReorder(QueuedRunReorder) on thread_id,
    "queued-run.cancel" => QueuedRunCancel(QueuedRunCancel) on thread_id,
    "queued-run.edit" => QueuedRunEdit(QueuedRunEdit) on thread_id,
    "runtime-request.respond" => RuntimeRequestRespond(RuntimeRequestRespond) on thread_id,
    "thread.user-input.dismiss" => ThreadUserInputDismiss(ThreadUserInputDismiss) on thread_id,
    "checkpoint.rollback" => CheckpointRollback(CheckpointRollback) on thread_id,
    "thread.fork" => ThreadFork(ThreadFork) on target_thread_id,
    "thread.merge_back" => ThreadMergeBack(ThreadMergeBack) on target_thread_id,
    "delegated_task.request" => DelegatedTaskRequest(DelegatedTaskRequest) on parent_thread_id,
    "delegated_task.wake-policy" => DelegatedTaskWakePolicy(DelegatedTaskWakePolicy) on parent_thread_id,
    "delegated_task.completion-delivery.acknowledge" => DelegatedTaskCompletionDeliveryAcknowledge(DelegatedTaskCompletionDeliveryAcknowledge) on parent_thread_id,
    "delegated_task.completion-delivery.dispose" => DelegatedTaskCompletionDeliveryDispose(DelegatedTaskCompletionDeliveryDispose) on parent_thread_id,
    "thread.created.record" => ThreadCreatedRecord(ThreadCreatedRecord) on parent_thread_id,
    "provider.switch" => ProviderSwitch(ProviderSwitch) on thread_id,
    // Server-only (`OrchestrationV2InternalCommand`): no client may send these to t3code.
    "thread.guest-share.open" => ThreadGuestShareOpen(ThreadGuestShareOpen) on thread_id,
    "thread.guest-share.redeem" => ThreadGuestShareRedeem(ThreadGuestShareRedeem) on thread_id,
    "thread.guest-message.submit" => ThreadGuestMessageSubmit(ThreadGuestMessageSubmit) on thread_id,
    "thread.project.adopt-folder" => ThreadProjectAdoptFolder(ThreadProjectAdoptFolder) on thread_id,
    "thread.pull-request-watch.checks-reopen" => ThreadPullRequestWatchChecksReopen(ThreadPullRequestWatchChecksReopen) on thread_id,
    "thread.pull-request-watch.sync" => ThreadPullRequestWatchSync(ThreadPullRequestWatchSync) on thread_id,
    "delegated_task.remote.request" => DelegatedTaskRemoteRequest(DelegatedTaskRemoteRequest) on parent_thread_id,
    "delegated_task.remote.complete" => DelegatedTaskRemoteComplete(DelegatedTaskRemoteComplete) on parent_thread_id,
    "delegated_task.remote.stop" => DelegatedTaskRemoteStop(DelegatedTaskRemoteStop) on parent_thread_id,
    "checkpoint.rollback.fail" => CheckpointRollbackFail(CheckpointRollbackFail) on thread_id,
    "runtime-request.delivery.fail" => RuntimeRequestDeliveryFail(RuntimeRequestDeliveryFail) on thread_id,
    "thread.interrupt.unresponsive" => ThreadInterruptUnresponsive(ThreadInterruptUnresponsive) on thread_id,
    "thread.background-work.settle" => ThreadBackgroundWorkSettle(ThreadBackgroundWorkSettle) on thread_id,
    "secret_request.record" => SecretRequestRecord(SecretRequestRecord) on thread_id,
    "link_request.record" => LinkRequestRecord(LinkRequestRecord) on thread_id,
}

impl Command {
    /// The command that parsed JSON holds.
    pub fn from_value(command: Value) -> serde_json::Result<Self> {
        Self::deserialize(command)
    }
}

/// A command of a type this build does not know: what every command names,
/// and the whole record as it was sent.
#[derive(Clone, Debug, PartialEq)]
pub struct OtherCommand {
    command_type: Box<str>,
    pub command_id: CommandId,
    pub thread_id: Option<ThreadId>,
    record: Map<String, Value>,
}

impl OtherCommand {
    /// The wire name of the command's type.
    pub fn type_name(&self) -> &str {
        &self.command_type
    }

    /// The whole record, `type` included.
    pub fn record(&self) -> &Map<String, Value> {
        &self.record
    }
}

impl Serialize for OtherCommand {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.record.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OtherCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let record = Map::<String, Value>::deserialize(deserializer)?;
        let text = |key: &'static str| match record.get(key) {
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(Value::Null) | None => Ok(None),
            Some(_) => Err(D::Error::custom(format_args!("`{key}` is not a string"))),
        };
        let command_type = text("type")?.ok_or_else(|| D::Error::missing_field("type"))?.into();
        let command_id = CommandId::from(text("commandId")?.ok_or_else(|| D::Error::missing_field("commandId"))?);
        let thread_id = text("threadId")?.map(ThreadId::from);
        Ok(Self { command_type, command_id, thread_id, record })
    }
}

// ---- the thread itself

command! {
    /// Stops the thread's active run.
    ThreadStop {
        command_id: CommandId,
        thread_id: ThreadId,
        reason?: String,
        /// False when the caller stops the thread's delegated tasks itself.
        stop_delegated_tasks?: bool,
    }
}

command! {
    /// Creates a thread without sending it a message.
    ThreadCreate {
        created_by: Actor,
        creation_source: CreationSource,
        command_id: CommandId,
        thread_id: ThreadId,
        project_id: ProjectId,
        title: String,
        model_selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        branch: Option<String>,
        worktree_path: Option<String>,
        link_origin?: LinkOrigin,
        delegated_from?: LinkedThread,
        imported_native_thread?: ImportedNativeThread,
    }
}

/// The provider's own conversation a thread is created to continue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedNativeThread {
    #[serde(rename = "ref")]
    pub native_ref: ProviderRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ProviderThreadNativeMetadata>,
}

command! {
    ThreadArchive {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadUnarchive {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadDelete {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Delete only a thread with no runs and no messages.
        only_if_unused?: bool,
    }
}

command! {
    ThreadSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        expected_latest_run_id?: RunId,
        settled_at?: Timestamp,
    }
}

command! {
    /// Settles a thread nothing has happened on since `snapshotAt`.
    ThreadAutoSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Required by t3code; never read by domovoi-server, which accepts a command without it.
        snapshot_at?: Timestamp,
        settled_at?: Timestamp,
    }
}

string_enum! {
    /// Who un-settles or wakes a thread: only ever the user.
    ByUser {
        User = "user",
    }
}

command! {
    ThreadUnsettle {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Required by t3code; never read by domovoi-server, which accepts a command without it.
        reason?: ByUser,
    }
}

command! {
    ThreadSnooze {
        command_id: CommandId,
        thread_id: ThreadId,
        snoozed_until: Timestamp,
    }
}

command! {
    ThreadUnsnooze {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Required by t3code; never read by domovoi-server, which accepts a command without it.
        reason?: ByUser,
    }
}

command! {
    ThreadAutoSettleSet {
        command_id: CommandId,
        thread_id: ThreadId,
        enabled: bool,
    }
}

command! {
    ThreadPin {
        command_id: CommandId,
        thread_id: ThreadId,
        order_key?: String,
    }
}

command! {
    ThreadUnpin {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadTagSet {
        command_id: CommandId,
        thread_id: ThreadId,
        tag: String,
        tagged: bool,
    }
}

command! {
    ThreadPinReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        order_key: String,
    }
}

command! {
    ThreadActiveReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        order_key: String,
    }
}

command! {
    ThreadVisit {
        command_id: CommandId,
        thread_id: ThreadId,
        visited_at: Timestamp,
        /// The user opened the thread, as opposed to new activity on an open one.
        opened?: bool,
        search_query?: String,
    }
}

command! {
    ThreadMarkUnread {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadMetadataUpdate {
        command_id: CommandId,
        thread_id: ThreadId,
        title?: String,
        regenerate_title?: bool,
        branch??: String,
        worktree_path??: String,
        expected_worktree_path??: String,
        expected_branch??: String,
        /// Reject unless no message or run has landed on the thread.
        expected_empty?: bool,
        limit_recovery??: LimitRecoveryUpdate,
        /// Link (a record) or unlink (`null`) a pull request.
        linked_pull_request??: LinkedPullRequest,
    }
}

/// What to do about a run a usage limit stopped (`OrchestrationV2LimitRecoveryUpdate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecoveryUpdate {
    pub run_id: RunId,
    pub reset_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

command! {
    /// The text generation model's answer to a title regeneration.
    ThreadTitleRegenerationComplete {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: CommandId,
        title?: String,
        outcome?: TitleRegenerationOutcome,
    }
}

/// How a title regeneration ended (`ThreadTitleRegenerationOutcome`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRegenerationOutcome {
    pub status: TitleRegenerationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

command! {
    ThreadRuntimeModeSet {
        command_id: CommandId,
        thread_id: ThreadId,
        runtime_mode: RuntimeMode,
    }
}

command! {
    ThreadInteractionModeSet {
        command_id: CommandId,
        thread_id: ThreadId,
        interaction_mode: InteractionMode,
    }
}

command! {
    ThreadModelSelectionSet {
        command_id: CommandId,
        thread_id: ThreadId,
        model_selection: ModelSelection,
    }
}

command! {
    ProviderSwitch {
        command_id: CommandId,
        thread_id: ThreadId,
        model_selection: ModelSelection,
    }
}

command! {
    /// Moves a thread to the project rooted at the folder it already works in.
    ThreadProjectAdoptFolder {
        command_id: CommandId,
        thread_id: ThreadId,
        project_id: ProjectId,
    }
}

// ---- guests

command! {
    ThreadGuestShareOpen {
        command_id: CommandId,
        thread_id: ThreadId,
        share_id: GuestShareId,
        host: String,
        expires_at: Timestamp,
    }
}

command! {
    ThreadGuestShareRedeem {
        command_id: CommandId,
        thread_id: ThreadId,
        share_id: GuestShareId,
        guest_name: String,
    }
}

command! {
    ThreadGuestShareClose {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadGuestMessageSubmit {
        command_id: CommandId,
        thread_id: ThreadId,
        share_id: GuestShareId,
        guest_message_id: GuestMessageId,
        text: String,
        attachments?: Vec<ChatAttachment>,
    }
}

command! {
    ThreadGuestMessageDismiss {
        command_id: CommandId,
        thread_id: ThreadId,
        guest_message_id: GuestMessageId,
    }
}

// ---- pull requests

command! {
    ThreadPullRequestLink {
        command_id: CommandId,
        thread_id: ThreadId,
        host: String,
        repository: String,
        number: u64,
        url: String,
        source: PullRequestLinkSource,
        repository_web_url?: String,
    }
}

command! {
    ThreadPullRequestUnlink {
        command_id: CommandId,
        thread_id: ThreadId,
        host: String,
        repository: String,
        number: u64,
    }
}

command! {
    ThreadPullRequestLinkSync {
        command_id: CommandId,
        thread_id: ThreadId,
        host: String,
        repository: String,
        number: u64,
        snapshot: PullRequestSnapshot,
        stack: Option<PullRequestStack>,
    }
}

command! {
    ThreadPullRequestWatch {
        command_id: CommandId,
        thread_id: ThreadId,
        host: String,
        repository: String,
        number: u64,
        watching: bool,
        link?: PullRequestToLink,
    }
}

/// The link to make along with a watch on a pull request not yet linked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestToLink {
    pub url: String,
    pub source: PullRequestLinkSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_web_url: Option<String>,
}

command! {
    ThreadPullRequestSync {
        command_id: CommandId,
        thread_id: ThreadId,
        project_id: ProjectId,
        snapshot_sequence: u64,
        expected: PullRequestSyncExpectation,
        branch_pull_request: Option<LinkedPullRequest>,
        linked_pull_request?: LinkedPullRequest,
    }
}

/// What a pull request sync read the thread as; it applies only if that still holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSyncExpectation {
    pub workspace_root: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub linked_pull_request: Option<LinkedPullRequest>,
    pub branch_pull_request: Option<LinkedPullRequest>,
}

command! {
    /// After a push the thread's watches forget whether their checks were running.
    ThreadPullRequestWatchChecksReopen {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    ThreadPullRequestWatchSync {
        command_id: CommandId,
        thread_id: ThreadId,
        host: String,
        repository: String,
        number: u64,
        started_at: Timestamp,
        watch: Option<PullRequestWatch>,
        checks_pending_before?: bool,
        ended_watch?: PullRequestWatch,
        others?: Vec<PullRequestWatchRead>,
        wake?: PullRequestWatchWake,
    }
}

/// Another watch of the thread recorded with a `thread.pull-request-watch.sync`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWatchRead {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub started_at: Timestamp,
    pub watch: Option<PullRequestWatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_pending_before: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_watch: Option<PullRequestWatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub includes_remarks: Option<bool>,
}

/// The message that tells the agent what a pull request watch saw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWatchWake {
    pub message_id: MessageId,
    pub text: String,
    pub notification: Notification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub includes_remarks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<PullRequestWakeSection>>,
}

// ---- messages and runs

command! {
    /// Sends a message to the thread's agent.
    MessageDispatch {
        notification?: Notification,
        /// Sends this pending guest message; the server uses the guest's text, not `text`.
        guest_message_id?: GuestMessageId,
        created_by: Actor,
        creation_source: CreationSource,
        scheduled_task_id?: ScheduledTaskId,
        sender_thread_id?: ThreadId,
        linked_sender?: LinkedThread,
        idle_compaction?: IdleCompaction,
        /// Start a new provider session without the thread's earlier context.
        fresh_context?: bool,
        command_id: CommandId,
        thread_id: ThreadId,
        message_id: MessageId,
        text: String,
        context?: MessageContext,
        attachments: Vec<ChatAttachment>,
        title_seed?: String,
        model_selection?: ModelSelection,
        source_plan_ref?: SourcePlanRef,
        restart_continuation_of_run_id?: RunId,
        usage_limit_continuation_of_run_id?: RunId,
        manual_continuation_of_run_id?: RunId,
        steer_follow_up_of_run_id?: RunId,
        steer_follow_up_after_run_ordinal?: u64,
        usage_limit_recovery_request_id?: CommandId,
        delivery_intent?: DeliveryIntent,
        delegated_completion?: DelegatedCompletionReport,
        dispatch_mode: DispatchMode,
    }
}

impl MessageDispatch {
    /// A message with nothing attached and no options set, which waits its
    /// turn behind whatever the thread is doing.
    pub fn new(
        command_id: impl Into<CommandId>,
        thread_id: impl Into<ThreadId>,
        message_id: impl Into<MessageId>,
        text: impl Into<String>,
        created_by: Actor,
        creation_source: CreationSource,
    ) -> Self {
        Self {
            notification: None,
            guest_message_id: None,
            created_by,
            creation_source,
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            fresh_context: None,
            command_id: command_id.into(),
            thread_id: thread_id.into(),
            message_id: message_id.into(),
            text: text.into(),
            context: None,
            attachments: Vec::new(),
            title_seed: None,
            model_selection: None,
            source_plan_ref: None,
            restart_continuation_of_run_id: None,
            usage_limit_continuation_of_run_id: None,
            manual_continuation_of_run_id: None,
            steer_follow_up_of_run_id: None,
            steer_follow_up_after_run_ordinal: None,
            usage_limit_recovery_request_id: None,
            delivery_intent: None,
            delegated_completion: None,
            dispatch_mode: DispatchMode::QueueAfterActive,
        }
    }
}

string_enum! {
    /// How the sender would like a message delivered to a busy thread.
    DeliveryIntent {
        Auto = "auto",
        Steer = "steer",
        Restart = "restart",
    }
}

/// What a message does when the thread already has a run going.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DispatchMode {
    /// The run waits for its workspace to be prepared.
    DeferStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_strategy: Option<WorkspaceStrategy>,
    },
    /// The message joins the turn `targetRunId` is running.
    SteerActive { target_run_id: RunId },
    /// `targetRunId` is stopped and the message goes next.
    RestartActive { target_run_id: RunId },
    QueueAfterActive,
    StartImmediately,
}

command! {
    PreparedRunRelease {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    }
}

command! {
    PreparedRunProgress {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        phase: PreparationPhase,
    }
}

string_enum! {
    /// What is being prepared for a run that waits for its workspace.
    PreparationPhase {
        Worktree = "worktree",
        Setup = "setup",
    }
}

command! {
    PreparedRunFail {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        failure: ProviderFailure,
    }
}

command! {
    PreparedRunRetry {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    }
}

command! {
    NotificationDeliveryAccept {
        command_id: CommandId,
        thread_id: ThreadId,
        message_id: MessageId,
    }
}

command! {
    ProviderSessionDetach {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_session_id: ProviderSessionId,
        reason?: String,
    }
}

command! {
    SubagentStop {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Required by t3code; never read by domovoi-server, which accepts a command without it.
        run_id?: RunId,
        subagent_id: NodeId,
    }
}

command! {
    RunInterrupt {
        command_id: CommandId,
        thread_id: ThreadId,
        /// Required by t3code; never read by domovoi-server, which accepts a command without it.
        run_id?: RunId,
        reason?: String,
        /// What is queued then waits for the user's next message.
        hold_queue?: bool,
    }
}

command! {
    /// A queued message joins the turn `targetRunId` is running instead of waiting.
    QueuedMessagePromoteToSteer {
        command_id: CommandId,
        thread_id: ThreadId,
        queued_run_id: RunId,
        target_run_id: RunId,
    }
}

command! {
    /// Releases a queue a stop held.
    QueueResume {
        command_id: CommandId,
        thread_id: ThreadId,
    }
}

command! {
    QueuedRunStart {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    }
}

command! {
    /// Moves a queued run before another, or to the end (`null`).
    QueuedRunReorder {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        before_run_id: Option<RunId>,
    }
}

command! {
    QueuedRunCancel {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
    }
}

command! {
    QueuedRunEdit {
        context?: MessageContext,
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        text: String,
        /// Replaces all attachments; absent keeps them.
        attachments?: Vec<ChatAttachment>,
    }
}

command! {
    /// The user's answer to an approval or a question.
    RuntimeRequestRespond {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
        decision?: ApprovalDecision,
        feedback?: String,
        /// Answers keyed by question id. The schema leaves each answer's shape open.
        answers?: Map<String, Value>,
        attachments_by_question_id?: BTreeMap<String, Vec<ChatAttachment>>,
    }
}

command! {
    ThreadUserInputDismiss {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
    }
}

command! {
    CheckpointRollback {
        restore_files?: bool,
        command_id: CommandId,
        thread_id: ThreadId,
        scope_id: CheckpointScopeId,
        checkpoint_id: CheckpointId,
    }
}

command! {
    CheckpointRollbackFail {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: CommandId,
        message: String,
    }
}

command! {
    /// Delivering the answer to a runtime request failed on its last attempt.
    RuntimeRequestDeliveryFail {
        command_id: CommandId,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
    }
}

command! {
    /// The provider did not confirm a stop of this turn in time.
    ThreadInterruptUnresponsive {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    }
}

command! {
    ThreadBackgroundWorkSettle {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        reason?: String,
        subagent?: SettledSubagent,
    }
}

/// The subagent a `thread.background-work.settle` ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledSubagent {
    pub id: NodeId,
    pub stopped_at: Timestamp,
    pub outcome: SettledSubagentOutcome,
}

/// How a settled subagent ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledSubagentOutcome {
    pub status: SettledStatus,
    pub completed_at: Timestamp,
    pub result: Option<String>,
}

string_enum! {
    /// What a settled subagent ended as.
    SettledStatus {
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

// ---- forks

/// Where in the source thread a fork starts (`OrchestrationV2ThreadForkSourcePoint`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ForkSourcePoint {
    /// The latest run that finished.
    LatestStable,
    Run { run_id: RunId },
    /// The run that captured the checkpoint.
    Checkpoint { checkpoint_id: CheckpointId },
}

command! {
    /// Creates `targetThreadId` as a fork of `sourceThreadId`.
    ThreadFork {
        created_by: Actor,
        creation_source: CreationSource,
        command_id: CommandId,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        source_point: ForkSourcePoint,
        title?: String,
        side_chat?: bool,
        created_at?: Timestamp,
    }
}

command! {
    /// Hands a fork's work back to the thread it was forked from.
    ThreadMergeBack {
        created_by: Actor,
        creation_source: CreationSource,
        command_id: CommandId,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        source_point: ForkSourcePoint,
        created_at?: Timestamp,
    }
}

// ---- delegated tasks

command! {
    DelegatedTaskRequest {
        created_by: Actor,
        creation_source: CreationSource,
        command_id: CommandId,
        parent_thread_id: ThreadId,
        parent_run_id: RunId,
        parent_node_id: NodeId,
        task: String,
        title?: String,
        model_selection: ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        workspace_strategy?: WorkspaceStrategy,
        completion_wake?: CompletionWake,
        created_at?: Timestamp,
    }
}

command! {
    DelegatedTaskWakePolicy {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
        completion_wake: CompletionWake,
    }
}

command! {
    DelegatedTaskCompletionDeliveryAcknowledge {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
        observed_by_run_id: Option<RunId>,
    }
}

command! {
    DelegatedTaskCompletionDeliveryDispose {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
    }
}

command! {
    /// Records, on the parent, a thread an agent created.
    ThreadCreatedRecord {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        parent_run_id: RunId,
        parent_node_id: NodeId,
        target_thread_id: ThreadId,
        target_run_id: Option<RunId>,
    }
}

command! {
    /// Records a delegated task that runs in a linked environment.
    DelegatedTaskRemoteRequest {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        parent_run_id: RunId,
        parent_node_id: NodeId,
        task: String,
        title?: String,
        driver: ProviderDriverKind,
        model_selection: ModelSelection,
        remote_child: RemoteTaskChild,
        completion_wake?: CompletionWake,
    }
}

command! {
    /// Completes a delegated task that ran in a linked environment.
    DelegatedTaskRemoteComplete {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
        status: RemoteTaskEnd,
        result: String,
        stopped_there?: StoppedThere,
        /// The stop generation this report acknowledges.
        stop_count?: u64,
    }
}

string_enum! {
    /// What a delegated task's thread in a linked environment ended as.
    RemoteTaskEnd {
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
        Interrupted = "interrupted",
    }
}

string_enum! {
    /// How the run of a remote task stopped there.
    StoppedThere {
        /// The run there ended by itself.
        RanOut = "ran-out",
        /// A stop from here reached it.
        Stopped = "stopped",
    }
}

command! {
    /// Owes a stop to the thread there of a remote task that already has its result.
    DelegatedTaskRemoteStop {
        command_id: CommandId,
        parent_thread_id: ThreadId,
        task_id: NodeId,
    }
}

// ---- requests an agent makes of the user

command! {
    SecretRequestRecord {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        node_id: NodeId,
        turn_item_id: TurnItemId,
        label: String,
        reason: String,
        url?: String,
        placeholder?: String,
        delivery?: SecretDelivery,
        secret_status: SecretRequestStatus,
    }
}

command! {
    LinkRequestRecord {
        command_id: CommandId,
        thread_id: ThreadId,
        run_id: RunId,
        node_id: NodeId,
        turn_item_id: TurnItemId,
        url?: String,
        reason: String,
        environment_id?: EnvironmentId,
        label?: String,
        hint?: String,
        requested_access?: McpClientAccess,
        link_status: LinkRequestStatus,
        linked_environment_id?: EnvironmentId,
        linked_label?: String,
        linked_access?: McpClientAccess,
        failure?: String,
        wake?: RequestWake,
    }
}

/// The message that tells the agent how a request card ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestWake {
    pub message_id: MessageId,
    pub text: String,
    pub notification: Notification,
}

// ---- launching a thread, and what the two RPCs answer

/// What `orchestration.launchThread` takes (`OrchestrationV2ThreadLaunchInput`),
/// with two fields of domovoi-server's own, used by the launches it makes itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchInput {
    pub command_id: CommandId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_source: Option<CreationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_existing_thread: Option<bool>,
    pub project_id: ProjectId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_title: Option<bool>,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace_strategy: WorkspaceStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<InitialMessage>,
    /// Not in t3code's schema: who starts the thread, when not the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Actor>,
    /// Not in t3code's schema: the thread whose agent delegated this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<LinkedThread>,
}

/// The message a launched thread starts with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
}

/// What `orchestration.launchThread` answers (`OrchestrationV2ThreadLaunchResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLaunchResult {
    pub thread_id: ThreadId,
    pub projection: ThreadProjection,
    pub resumed: bool,
    /// The sequence the initial message was committed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

/// What `orchestration.dispatchCommand` answers (`OrchestrationV2DispatchCommandResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchCommandResult {
    pub sequence: u64,
}

/// Declares a tagged error as t3code's `Schema.TaggedError` writes it: its
/// fields with a `_tag` naming it.
macro_rules! tagged_error {
    ($(#[$meta:meta])* $name:ident as $tag:literal { $($(#[$field_meta:meta])* $field:ident: $ty:ty,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "_tag", rename = $tag, rename_all = "camelCase")]
        pub struct $name { $($(#[$field_meta])* pub $field: $ty,)+ }
    };
}

tagged_error! {
    /// How `orchestration.dispatchCommand` fails (`OrchestrationV2DispatchCommandError`).
    /// The schema's `cause`, a defect for logs, is not carried.
    DispatchCommandError as "OrchestrationV2DispatchCommandError" {
        command_id: CommandId,
        command_type: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    }
}

tagged_error! {
    /// How `orchestration.launchThread` fails (`OrchestrationV2ThreadLaunchError`).
    /// The schema's `cause`, a defect for logs, is not carried.
    ThreadLaunchError as "OrchestrationV2ThreadLaunchError" {
        command_id: CommandId,
        project_id: ProjectId,
        message: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_command_names_its_id_its_thread_and_its_type() {
        let visit = Command::from_value(json!({ "type": "thread.visit", "commandId": "c1", "threadId": "t1", "visitedAt": "2026-01-02T03:04:05.678Z" })).unwrap();
        assert_eq!((visit.type_name(), visit.command_id().as_str(), visit.thread_id().map(ThreadId::as_str)), ("thread.visit", "c1", Some("t1")));
        let fork = json!({ "type": "thread.fork", "commandId": "c2", "sourceThreadId": "a", "targetThreadId": "b", "sourcePoint": { "type": "latest_stable" },
            "createdBy": "user", "creationSource": "web" });
        let decoded = Command::from_value(fork.clone()).unwrap();
        assert_eq!(decoded.thread_id().map(ThreadId::as_str), Some("b"), "a fork acts on the thread it creates");
        assert_eq!(json!(decoded), fork);
        let task = Command::from_value(json!({ "type": "delegated_task.remote.stop", "commandId": "c3", "parentThreadId": "p", "taskId": "n" })).unwrap();
        assert_eq!(task.thread_id().map(ThreadId::as_str), Some("p"));
    }

    #[test]
    fn an_unknown_command_is_kept_whole_and_a_known_one_decodes_in_full() {
        let sent = json!({ "type": "thread.teleport", "commandId": "c1", "threadId": "t1", "to": { "x": 1 } });
        let other = Command::from_value(sent.clone()).unwrap();
        assert!(matches!(other, Command::Other(_)));
        assert_eq!((other.type_name(), other.command_id().as_str(), other.thread_id().map(ThreadId::as_str)), ("thread.teleport", "c1", Some("t1")));
        assert_eq!(json!(other), sent);
        assert!(Command::from_value(json!({ "type": "thread.teleport", "commandId": "c1" })).unwrap().thread_id().is_none());
        // A known type with a field missing is an error, not an `Other`.
        let error = Command::from_value(json!({ "type": "thread.visit", "commandId": "c1", "threadId": "t1" })).unwrap_err();
        assert!(error.to_string().contains("visitedAt"), "{error}");
        assert!(Command::from_value(json!({ "type": "thread.teleport" })).is_err(), "every command has an id");
        assert!(Command::from_value(json!({ "commandId": "c1" })).is_err(), "and a type");
    }

    #[test]
    fn absent_and_null_stay_distinct_where_the_schema_tells_them_apart() {
        for fields in [json!({}), json!({ "branch": null }), json!({ "branch": "main", "worktreePath": null })] {
            let mut sent = json!({ "type": "thread.metadata.update", "commandId": "c1", "threadId": "t1" });
            sent.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            assert_eq!(json!(Command::from_value(sent.clone()).unwrap()), sent);
        }
        let reorder = json!({ "type": "queued-run.reorder", "commandId": "c1", "threadId": "t1", "runId": "r1", "beforeRunId": null });
        assert_eq!(json!(Command::from_value(reorder.clone()).unwrap()), reorder);
    }

    #[test]
    fn errors_carry_their_tag() {
        let error = DispatchCommandError { command_id: "c1".into(), command_type: "thread.visit".into(), message: "Unknown thread t1".into(), detail: None };
        let sent = json!({ "_tag": "OrchestrationV2DispatchCommandError", "commandId": "c1", "commandType": "thread.visit", "message": "Unknown thread t1" });
        assert_eq!(json!(error), sent);
        assert_eq!(serde_json::from_value::<DispatchCommandError>(sent).unwrap(), error);
        let error = ThreadLaunchError { command_id: "c1".into(), project_id: "p".into(), message: "Unknown project.".into() };
        assert_eq!(json!(error), json!({ "_tag": "OrchestrationV2ThreadLaunchError", "commandId": "c1", "projectId": "p", "message": "Unknown project." }));
    }
}
