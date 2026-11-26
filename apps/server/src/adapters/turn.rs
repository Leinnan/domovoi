//! Records one provider turn as orchestration events, independent of which
//! CLI produced it. Adapters translate native activity into calls here; ids,
//! statuses and record shapes follow a recorded t3code session.

#![expect(clippy::disallowed_types, reason = "a dynamic tool's input and output, which the schema leaves open; the bundled provider-capabilities JSON, typed on read")]

use crate::orchestration::commands::{Answers, provider_session_id};
use crate::orchestration::events::{Kept, Origin, enc};
use crate::store::Store;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use domovoi_model::typed::{
    Actor, ApprovalOption, CompletionWake, ContextUsage, ContextWindowUsage, ConversationMessage, CreationSource, EventBody, ExecutionNode,
    FileChangeDetail, MessageId, MessageRole, NativeRefStrength, NodeId, NodeKind, NodeStatus, PendingBackgroundTask, PlanArtifact,
    PlanId, PlanStatus, PlanStep, ProposedPlan, ProviderCapabilities, ProviderDriverKind, ProviderFailureClass, ProviderInstanceId, ProviderRef,
    ProviderRequestKind, ProviderSession, ProviderSessionId, ProviderSessionStatus, ProviderThread, ProviderThreadId,
    ProviderThreadStatus, ProviderTurn, ProviderTurnId, ProviderTurnStatus, ResponseCapability, ResponseMode, Run, RunAttempt, RunAttemptStatus,
    RunId, RunStatus, RuntimeRequest, RuntimeRequestId, RuntimeRequestKind, RuntimeRequestStatus, ThreadId, Timestamp, TodoList,
    TurnItem, TurnItemId, TurnItemStatus, UserInputQuestion, WebSearchResult,
};

const STREAM_FLUSH: Duration = Duration::from_millis(120);
/// A long streaming text is sent less often: each update carries all of it.
const STREAM_FLUSH_LONG: Duration = Duration::from_millis(250);
const LONG_TEXT: usize = 8 * 1024;

fn flush_interval(len: usize) -> Duration {
    if len > LONG_TEXT { STREAM_FLUSH_LONG } else { STREAM_FLUSH }
}

/// What a tool, reasoning or system item shows.
pub enum Work {
    /// A shell command, with its output and exit code once it has them.
    Command { input: String, output: Option<String>, exit_code: Option<i64> },
    /// An edit to files: the old and new text (`texts`), or a unified patch.
    FileChange { file_name: String, changes: Vec<FileChangeDetail>, texts: Option<(String, String)>, patch: Option<String>, error: Option<String> },
    /// A call to a tool with no item type of its own.
    Tool { name: String, input: Value, output: Option<Value>, viewed_image_path: Option<String> },
    WebSearch { patterns: Vec<String>, results: Option<Vec<WebSearchResult>> },
    Reasoning { text: String, streaming: bool },
    /// The provider compacted its conversation.
    Compaction { before_token_count: Option<u64> },
    /// A note in the timeline.
    Notice { message: String },
}

impl Work {
    /// The kind of execution node this work runs as.
    fn node_kind(&self) -> NodeKind {
        match self {
            Work::Command { .. } | Work::FileChange { .. } | Work::Tool { .. } | Work::WebSearch { .. } => NodeKind::ToolCall,
            Work::Reasoning { .. } => NodeKind::Reasoning,
            Work::Compaction { .. } | Work::Notice { .. } => NodeKind::System,
        }
    }
}

/// A plan the agent wrote.
pub enum Plan {
    /// A proposal in markdown, for the user to accept.
    Proposed { markdown: String },
    /// The steps the agent set itself.
    Todo { steps: Vec<PlanStep>, explanation: Option<String> },
}

/// The context window's use after a provider's model call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowUse {
    pub used_tokens: u64,
    /// The window's size, where the provider says it.
    pub max_tokens: Option<u64>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

/// How a provider turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Interrupted,
    Failed,
}

impl TurnEnd {
    /// How a turn a provider reports as `status` ended: `completed`,
    /// `interrupted`, or failed for anything else.
    pub fn named(status: &str) -> Self {
        match status {
            "completed" => TurnEnd::Completed,
            "interrupted" => TurnEnd::Interrupted,
            _ => TurnEnd::Failed,
        }
    }
}

/// The message of a reply that ended without one: a reply's message is
/// written when the reply is whole, so a turn the server did not see end (it
/// stopped mid-reply) left only the turn item. `shown` is that item, closed.
pub(crate) fn message_of_reply(shown: &domovoi_model::typed::AssistantMessageItem, at: &Timestamp) -> ConversationMessage {
    ConversationMessage {
        notification: None,
        created_by: Actor::Agent,
        creation_source: CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        id: shown.message_id.clone(),
        thread_id: shown.thread_id.clone(),
        run_id: shown.run_id.clone(),
        node_id: shown.node_id.clone(),
        role: MessageRole::Assistant,
        text: shown.text.clone(),
        context: None,
        attachments: Vec::new(),
        streaming: false,
        created_at: shown.started_at.clone().unwrap_or_else(|| at.clone()),
        updated_at: at.clone(),
        delegated_completion: None,
    }
}

/// The capabilities a session of `driver` reports, from the bundled defaults:
/// read and decoded once, not for every session a turn records.
fn capabilities_of(driver: &str) -> Option<ProviderCapabilities> {
    static BY_DRIVER: std::sync::LazyLock<HashMap<String, ProviderCapabilities>> = std::sync::LazyLock::new(|| {
        let Value::Object(by_driver) = crate::defaults::provider_capabilities() else { return HashMap::new() };
        by_driver.into_iter().filter_map(|(driver, capabilities)| Some((driver, serde_json::from_value(capabilities).ok()?))).collect()
    });
    BY_DRIVER.get(driver).cloned()
}

/// The newest streaming update held back by the throttle; the turn's end
/// sends it, so a stop doesn't lose the tail.
enum Held {
    Assistant { text: String, phase: Option<String> },
    Item(Work),
}

/// The fields every turn item carries.
pub(crate) struct Shared {
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub parent_item_id: Option<TurnItemId>,
    pub ordinal: u64,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

impl Shared {
    /// The shared fields of `item`.
    pub(super) fn of(item: &TurnItem) -> Self {
        Shared {
            id: item.id().clone(),
            thread_id: item.thread_id().clone(),
            run_id: item.run_id().cloned(),
            node_id: item.node_id().cloned(),
            provider_thread_id: item.provider_thread_id().cloned(),
            provider_turn_id: item.provider_turn_id().cloned(),
            native_item_ref: item.native_item_ref().cloned(),
            parent_item_id: item.parent_item_id().cloned(),
            ordinal: item.ordinal(),
            status: item.status().clone(),
            title: item.title().map(str::to_owned),
            started_at: item.started_at().cloned(),
            completed_at: item.completed_at().cloned(),
            updated_at: item.updated_at().clone(),
        }
    }

    /// Moves the item to `status` now, noting when it ended if this ends it.
    pub(super) fn at(mut self, status: TurnItemStatus) -> Self {
        let ts = now();
        if ended(&status) && self.completed_at.is_none() {
            self.completed_at = Some(ts.clone());
        }
        self.status = status;
        self.updated_at = ts;
        self
    }
}

/// A turn item of one type: its shared fields, then its own.
macro_rules! turn_item {
    ($variant:ident($record:ident), $shared:expr, { $($field:ident: $value:expr),* $(,)? }) => {{
        let shared: $crate::adapters::turn::Shared = $shared;
        domovoi_model::typed::TurnItem::$variant(Box::new(domovoi_model::typed::$record {
            deferred_work: None,
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: shared.id,
            thread_id: shared.thread_id,
            run_id: shared.run_id,
            node_id: shared.node_id,
            provider_thread_id: shared.provider_thread_id,
            provider_turn_id: shared.provider_turn_id,
            native_item_ref: shared.native_item_ref,
            parent_item_id: shared.parent_item_id,
            ordinal: shared.ordinal,
            status: shared.status,
            title: shared.title,
            started_at: shared.started_at,
            completed_at: shared.completed_at,
            updated_at: shared.updated_at,
            $($field: $value),*
        }))
    }};
}
pub(crate) use turn_item;

/// Ends a turn item of a type this module or `subagents` writes: its status,
/// when it ended, and no longer streaming if it streams.
pub(super) fn end_item(item: &mut TurnItem, status: TurnItemStatus, at: &Timestamp) {
    macro_rules! end {
        ($record:ident) => {{
            $record.status = status;
            $record.completed_at = Some(at.clone());
            $record.updated_at = at.clone();
        }};
    }
    match item {
        TurnItem::AssistantMessage(r) => {
            r.streaming = false;
            end!(r)
        }
        TurnItem::Reasoning(r) => {
            r.streaming = false;
            end!(r)
        }
        TurnItem::ProposedPlan(r) => {
            r.streaming = false;
            end!(r)
        }
        TurnItem::TodoList(r) => end!(r),
        TurnItem::UserInputRequest(r) => end!(r),
        TurnItem::ApprovalRequest(r) => end!(r),
        TurnItem::CommandExecution(r) => end!(r),
        TurnItem::FileChange(r) => end!(r),
        TurnItem::WebSearch(r) => end!(r),
        TurnItem::DynamicTool(r) => end!(r),
        TurnItem::Compaction(r) => end!(r),
        TurnItem::SystemNotice(r) => end!(r),
        TurnItem::Subagent(r) => end!(r),
        TurnItem::UserMessage(r) => end!(r),
        _ => {}
    }
}

pub struct Turn {
    store: Store,
    pub thread_id: String,
    pub run: Run,
    pub driver: String,
    pub instance: String,
    pub session_id: String,
    /// The run's root node, which every node of the turn hangs from.
    root: NodeId,
    provider_thread: ProviderThread,
    provider_turn: Option<ProviderTurn>,
    items: HashMap<String, TurnItem>,
    nodes: HashMap<String, ExecutionNode>,
    requests: HashMap<String, RuntimeRequest>,
    last_flush: HashMap<String, Instant>,
    held: HashMap<String, Held>,
    /// The last ordinal taken in the run's part of the timeline.
    last_ordinal: Option<u64>,
    /// A usage limit stopped the turn: it fails as `usage_limit`, resetting then.
    usage_limited: Option<Option<Timestamp>>,
}

pub(super) fn now() -> Timestamp {
    Timestamp::from(crate::time_fmt::iso_now())
}

pub(super) fn native_ref(driver: &str, id: &str) -> ProviderRef {
    ProviderRef { driver: ProviderDriverKind::from(driver), native_id: Some(id.to_owned()), strength: NativeRefStrength::Strong, fingerprint: None, ordinal: None }
}

/// Maps a t3code decision to whether the provider should proceed.
pub fn decision_allows(decision: &str) -> bool {
    matches!(decision, "accept" | "acceptForSession" | "acceptAlways")
}

/// The value of a closed set of wire strings spelled `name`; one the set
/// does not have is kept as it is spelled.
pub(crate) fn spelled<T: DeserializeOwned>(name: &str) -> Option<T> {
    T::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(name)).ok()
}

/// The kind of approval a provider adapter calls `name`.
pub fn approval_kind(name: &str) -> ProviderRequestKind {
    spelled(name).unwrap_or(ProviderRequestKind::Permission)
}

/// Whether an item or node in this status has ended.
pub(super) fn ended(status: &TurnItemStatus) -> bool {
    matches!(status, TurnItemStatus::Completed | TurnItemStatus::Failed | TurnItemStatus::Cancelled | TurnItemStatus::Interrupted)
}

/// The status of the node behind a turn item in `status`.
pub(super) fn node_status(status: &TurnItemStatus) -> NodeStatus {
    match status {
        TurnItemStatus::Idle => NodeStatus::Idle,
        TurnItemStatus::Pending => NodeStatus::Pending,
        TurnItemStatus::Running => NodeStatus::Running,
        TurnItemStatus::Waiting => NodeStatus::Waiting,
        TurnItemStatus::Completed => NodeStatus::Completed,
        TurnItemStatus::Failed => NodeStatus::Failed,
        TurnItemStatus::Cancelled => NodeStatus::Cancelled,
        TurnItemStatus::Interrupted => NodeStatus::Interrupted,
        TurnItemStatus::Other(other) => NodeStatus::Other(other.clone()),
    }
}

/// The kind of runtime request an approval of `kind` is.
fn request_kind(kind: &ProviderRequestKind) -> RuntimeRequestKind {
    match kind {
        ProviderRequestKind::Command => RuntimeRequestKind::Command,
        ProviderRequestKind::FileRead => RuntimeRequestKind::FileRead,
        ProviderRequestKind::FileChange => RuntimeRequestKind::FileChange,
        ProviderRequestKind::McpElicitation => RuntimeRequestKind::McpElicitation,
        ProviderRequestKind::Permission => RuntimeRequestKind::Permission,
        ProviderRequestKind::Other(other) => RuntimeRequestKind::Other(other.clone()),
    }
}

impl Turn {
    /// A usage limit stopped this turn; it resets at `reset_at` when known.
    pub fn usage_limited(&mut self, reset_at: Option<Timestamp>) {
        self.usage_limited = Some(reset_at);
    }

    /// Loads the run and its provider thread; `None` if either is missing.
    pub fn begin(store: Store, thread_id: &str, run_id: &str) -> Option<Self> {
        let run: Run = store.record(thread_id, run_id).ok()??;
        let provider_thread: ProviderThread = store.record(thread_id, run.provider_thread_id.as_ref()?.as_str()).ok()??;
        let root = run.root_node_id.clone()?;
        let instance = run.provider_instance_id.as_str().to_owned();
        let driver = provider_thread.driver.as_str().to_owned();
        // Imported provider threads have no session yet; their first run opens one.
        let session_id = provider_thread.provider_session_id.as_ref().map(ProviderSessionId::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
            .unwrap_or_else(|| provider_session_id(&instance, thread_id));
        let window = Self::ordinals_of(&run);
        let last_ordinal = store.last_item_ordinal(thread_id, window).unwrap_or_default();
        Some(Turn {
            store, thread_id: thread_id.into(), run, driver, instance, session_id, root, provider_thread,
            provider_turn: None, items: HashMap::new(), nodes: HashMap::new(), requests: HashMap::new(),
            last_flush: HashMap::new(), held: HashMap::new(), last_ordinal, usage_limited: None,
        })
    }

    /// The part of the timeline's ordinals that belongs to `run`.
    fn ordinals_of(run: &Run) -> std::ops::Range<u64> {
        let first = run.ordinal * 1_000_000;
        first..first + 1_000_000
    }

    /// Where the provider conversation should resume after a rollback, if set.
    pub fn conversation_head(&self) -> Option<String> {
        self.provider_thread.native_conversation_head_ref.as_ref().and_then(|head| head.native_id.clone())
    }

    /// Records the provider's cursor for this turn (Claude: the last assistant
    /// message uuid), which rollback resumes from.
    pub fn set_turn_cursor(&mut self, cursor: &str) {
        if let Some(t) = self.provider_turn.as_mut() {
            t.native_turn_ref = Some(native_ref(&self.driver, cursor));
        }
    }

    pub fn native_thread_id(&self) -> Option<String> {
        self.provider_thread.native_thread_ref.as_ref().and_then(|native| native.native_id.clone())
    }

    pub fn run_id(&self) -> String {
        self.run.id.as_str().to_owned()
    }

    fn origin(&self) -> Origin {
        Origin { driver: Some(self.driver.clone()), provider_instance_id: Some(self.instance.clone()), command_id: None }
    }

    fn emit(&self, event: EventBody) {
        self.emit_on(&self.thread_id, event);
    }

    /// Records an event on `thread_id` (this turn's thread, or a child
    /// thread it made, such as a subagent's).
    pub fn emit_on(&self, thread_id: &str, event: EventBody) {
        let event_type = event.event_type().to_owned();
        if let Err(e) = self.store.append_event(thread_id, event, &self.origin()) {
            tracing::error!(thread = %thread_id, "failed to record {event_type}: {e}");
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn root_node(&self) -> &NodeId {
        &self.root
    }

    pub fn provider_thread_id(&self) -> &ProviderThreadId {
        &self.provider_thread.id
    }

    pub fn provider_turn_id(&self) -> Option<&ProviderTurnId> {
        self.provider_turn.as_ref().map(|turn| &turn.id)
    }

    /// The next turn-item ordinal of this run.
    pub fn alloc_ordinal(&mut self) -> u64 {
        let next = self.last_ordinal.map_or(Self::ordinals_of(&self.run).start + 1, |last| last + 1);
        self.last_ordinal = Some(next);
        next
    }

    /// Edits a record of this turn's thread in one transaction, so a change
    /// made meanwhile by a command or the run's finalization is kept. Returns
    /// the record as written, or `None` if there is no such record.
    fn update<R: Kept>(&self, id: &str, change: impl FnOnce(&mut R)) -> Option<R> {
        match self.store.update_record(&self.thread_id, id, &self.origin(), change) {
            Ok(written) => written,
            Err(e) => {
                tracing::error!(thread = %self.thread_id, "failed to record a change to {id}: {e}");
                None
            }
        }
    }

    pub fn session(&self, status: ProviderSessionStatus, cwd: &str, model: &str, last_error: Option<&str>) {
        let Some(capabilities) = capabilities_of(&self.driver) else {
            tracing::error!(thread = %self.thread_id, "no capabilities to report for a {} session", self.driver);
            return;
        };
        let ts = now();
        let created_at = self.store.record::<ProviderSession>(&self.thread_id, &self.session_id).ok().flatten().map_or_else(|| ts.clone(), |s| s.created_at);
        self.emit(EventBody::ProviderSessionUpdated(Box::new(ProviderSession {
            id: ProviderSessionId::from(self.session_id.as_str()),
            driver: ProviderDriverKind::from(self.driver.as_str()),
            provider_instance_id: ProviderInstanceId::from(self.instance.as_str()),
            status,
            cwd: cwd.to_owned(),
            model: Some(model.to_owned()),
            capabilities,
            extension_statuses: Some(Vec::new()),
            created_at,
            updated_at: ts,
            last_error: last_error.map(str::to_owned),
        })));
    }

    pub fn thread_status(&mut self, status: ProviderThreadStatus, native_thread_id: Option<&str>) {
        let pt = &mut self.provider_thread;
        pt.status = status;
        pt.provider_session_id = Some(ProviderSessionId::from(self.session_id.as_str()));
        if let Some(n) = native_thread_id {
            pt.native_thread_ref = Some(native_ref(&self.driver, n));
        }
        pt.pending_background_tasks.get_or_insert_default();
        pt.context_usage.get_or_insert_default();
        pt.native_metadata.get_or_insert_default();
        pt.goal.get_or_insert_default();
        pt.updated_at = now();
        self.emit(EventBody::ProviderThreadUpdated(Box::new(self.provider_thread.clone())));
    }

    /// Work the provider keeps doing after its reply (background subagents):
    /// the provider thread's `pendingBackgroundTasks`.
    pub fn background_tasks(&mut self, tasks: Vec<PendingBackgroundTask>) {
        if self.provider_thread.pending_background_tasks.as_ref() == Some(&tasks) {
            return;
        }
        self.provider_thread.pending_background_tasks = Some(tasks);
        let status = self.provider_thread.status.clone();
        self.thread_status(status, None);
    }

    pub fn turn_started(&mut self, native_turn_id: &str) {
        if self.provider_turn.is_some() {
            return;
        }
        let ts = now();
        let start = |run: &mut Run| {
            run.status = RunStatus::Running;
            run.started_at = Some(ts.clone());
        };
        self.run = match self.update(self.run.id.as_str(), start) {
            Some(run) => run,
            None => {
                let mut run = self.run.clone();
                start(&mut run);
                self.emit(EventBody::RunUpdated(Box::new(run.clone())));
                run
            }
        };
        let native_thread = self.native_thread_id();
        if let Some(attempt) = &self.run.active_attempt_id {
            self.update(attempt.as_str(), |attempt: &mut RunAttempt| {
                attempt.status = RunAttemptStatus::Running;
                attempt.started_at = Some(ts.clone());
                if let Some(native_thread) = native_thread {
                    attempt.native_thread_id = Some(native_thread);
                }
            });
        }
        self.update(self.root.as_str(), |node: &mut ExecutionNode| {
            node.status = NodeStatus::Running;
            node.started_at = Some(ts.clone());
        });
        let turn = ProviderTurn {
            id: ProviderTurnId::from(format!("provider-turn:provider:{}:native-turn:{}", self.driver, enc(native_turn_id))),
            provider_thread_id: self.provider_thread.id.clone(),
            node_id: self.root.clone(),
            run_attempt_id: self.run.active_attempt_id.clone(),
            native_turn_ref: Some(native_ref(&self.driver, native_turn_id)),
            ordinal: self.provider_thread.last_run_ordinal.unwrap_or(1),
            status: ProviderTurnStatus::Running,
            started_at: Some(ts),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
            model: None,
        };
        self.provider_turn = Some(turn.clone());
        self.emit(EventBody::ProviderTurnUpdated(Box::new(turn)));
    }

    /// The context window's use after the provider's latest model call. The
    /// provider turn and the provider thread keep the latest, and both are
    /// written when the turn ends: nobody reads it before then, and a
    /// provider reports it after every model call.
    pub fn token_usage(&mut self, usage: WindowUse) {
        let Some(turn) = self.provider_turn.as_mut() else { return };
        turn.token_usage = Some(ContextWindowUsage {
            used_tokens: usage.used_tokens,
            max_tokens: Some(usage.max_tokens),
            input_tokens: Some(usage.input_tokens),
            cached_input_tokens: Some(usage.cached_input_tokens),
            output_tokens: Some(usage.output_tokens),
            reasoning_output_tokens: Some(usage.reasoning_output_tokens),
            prompt_cache_ttl_ms: None,
            auto_compact_threshold: None,
            prompt_cache_expires_at: None,
            updated_at: now(),
        });
        // A window of unknown size is left out, never written as none.
        self.provider_thread.context_usage = Some(Some(ContextUsage {
            used_tokens: usage.used_tokens,
            total_processed_tokens: None,
            max_tokens: usage.max_tokens.filter(|max| *max > 0),
            input_tokens: Some(usage.input_tokens),
            cached_input_tokens: Some(usage.cached_input_tokens),
            output_tokens: Some(usage.output_tokens),
            reasoning_output_tokens: Some(usage.reasoning_output_tokens),
            last_used_tokens: None,
            last_input_tokens: None,
            last_cached_input_tokens: None,
            last_output_tokens: None,
            last_reasoning_output_tokens: None,
            tool_uses: None,
            duration_ms: None,
            compacts_automatically: None,
            auto_compact_threshold: None,
            cost: None,
        }));
    }

    fn native_item_id(&self, prefix: &str, native_id: &str) -> String {
        format!("{prefix}:provider:{}:native-item:{}", self.driver, enc(native_id))
    }

    /// Brings the node of the native item `native_id` to `status`; the node's id.
    fn node_in(&mut self, native_id: &str, kind: NodeKind, status: NodeStatus, node_id: String) -> NodeId {
        self.node_for(native_id, kind, status, Some(self.root.clone()), None, node_id).id
    }

    /// The node of the native item `native_id` in `status`: the one the
    /// turn already holds, or a new one with the given id, kind and place.
    ///
    /// An item's node is kept by the turn only: its id is what the item, its
    /// plan and its requests are told apart by, and where a request hangs
    /// under its item. It is not recorded. Nothing reads a node but a run's
    /// root and a subagent's (which are recorded where they are made), and a
    /// node per item was a second record written for everything an agent did.
    fn node_for(
        &mut self,
        native_id: &str,
        kind: NodeKind,
        status: NodeStatus,
        parent: Option<NodeId>,
        request_id: Option<RuntimeRequestId>,
        node_id: String,
    ) -> ExecutionNode {
        let ts = now();
        let mut node = self.nodes.remove(native_id).unwrap_or_else(|| ExecutionNode {
            id: NodeId::from(node_id),
            thread_id: ThreadId::from(self.thread_id.as_str()),
            run_id: Some(self.run.id.clone()),
            parent_node_id: parent,
            root_node_id: self.root.clone(),
            kind,
            status: status.clone(),
            counts_for_run: false,
            provider_thread_id: Some(self.provider_thread.id.clone()),
            provider_turn_id: self.provider_turn_id().cloned(),
            native_item_ref: Some(native_ref(&self.driver, native_id)),
            runtime_request_id: request_id,
            checkpoint_scope_id: None,
            started_at: Some(ts.clone()),
            completed_at: None,
        });
        if matches!(status, NodeStatus::Completed | NodeStatus::Failed | NodeStatus::Cancelled | NodeStatus::Interrupted) && node.completed_at.is_none() {
            node.completed_at = Some(ts);
        }
        node.status = status;
        self.nodes.insert(native_id.to_owned(), node.clone());
        node
    }

    /// The shared fields of the item kept under `key`, moved to `status`: the
    /// item's as already written, or a new item's with the next ordinal.
    fn shared(&mut self, key: &str, id: String, node_id: &NodeId, native_id: &str, status: TurnItemStatus) -> Shared {
        let shared = match self.items.get(key) {
            Some(item) => Shared::of(item),
            None => {
                let ts = now();
                Shared {
                    id: TurnItemId::from(id),
                    thread_id: ThreadId::from(self.thread_id.as_str()),
                    run_id: Some(self.run.id.clone()),
                    node_id: Some(node_id.clone()),
                    provider_thread_id: Some(self.provider_thread.id.clone()),
                    provider_turn_id: self.provider_turn_id().cloned(),
                    native_item_ref: Some(native_ref(&self.driver, native_id)),
                    parent_item_id: None,
                    ordinal: self.alloc_ordinal(),
                    status: status.clone(),
                    title: None,
                    started_at: Some(ts.clone()),
                    completed_at: None,
                    updated_at: ts,
                }
            }
        };
        shared.at(status)
    }

    /// Creates or updates a tool, reasoning or system item and its execution node.
    pub fn item(&mut self, native_id: &str, work: Work, status: TurnItemStatus) {
        self.together(|turn| turn.write_item(native_id, work, status));
    }

    /// Runs `write` with its events committed together: an update is one
    /// transaction, and reaches the thread cache and subscribers whole.
    fn together(&mut self, write: impl FnOnce(&mut Self)) {
        let db = self.store.db().clone();
        // Each event that fails to record is logged where it is written.
        let _ = db.batch(|| -> rusqlite::Result<()> {
            write(self);
            Ok(())
        });
    }

    fn write_item(&mut self, native_id: &str, work: Work, status: TurnItemStatus) {
        // This state supersedes a streaming update the throttle held back.
        self.held.remove(native_id);
        let node_id = self.node_in(native_id, work.node_kind(), node_status(&status), self.native_item_id("node", native_id));
        let shared = self.shared(native_id, self.native_item_id("turn-item", native_id), &node_id, native_id, status);
        let item = self.work_item(shared, work, self.items.get(native_id));
        self.put_item(native_id, item);
    }

    /// Records the new version of the turn item kept under `key`. One that is
    /// the last version with more text (a reply, reasoning or a command's
    /// output as it streams) is stored whole like any other, and sent to
    /// those watching the thread as the text added.
    fn put_item(&mut self, key: &str, mut item: TurnItem) {
        let grew_from = self.items.remove(key).and_then(|earlier| item.grew_from(earlier));
        self.items.insert(key.to_owned(), item.clone());
        let recorded = match grew_from {
            Some(at) => self.store.append_grown_item(&self.thread_id, item, at, &self.origin()),
            None => self.store.append_event(&self.thread_id, EventBody::TurnItemUpdated(item), &self.origin()),
        };
        if let Err(e) = recorded {
            tracing::error!(thread = %self.thread_id, "failed to record turn-item.updated: {e}");
        }
    }

    /// The turn item that shows `work`. What `work` does not say (a command's
    /// output before it has any) stays as `previous`, the item's last version, had it.
    pub(super) fn work_item(&self, shared: Shared, work: Work, previous: Option<&TurnItem>) -> TurnItem {
        match work {
            Work::Command { input, output, exit_code } => {
                let previous = match previous {
                    Some(TurnItem::CommandExecution(previous)) => Some(previous),
                    _ => None,
                };
                turn_item!(CommandExecution(CommandExecutionItem), shared, {
                    input: input,
                    output: output.or_else(|| previous.and_then(|p| p.output.clone())),
                    output_omitted: None,
                    output_indicates_failure: None,
                    exit_code: exit_code.or_else(|| previous.and_then(|p| p.exit_code)),
                    monitor: None,
                    completion_wake: Some(CompletionWake::Always),
                })
            }
            Work::FileChange { file_name, changes, texts, patch, error } => {
                let previous = match previous {
                    Some(TurnItem::FileChange(previous)) => Some(previous),
                    _ => None,
                };
                let (old_str, new_str) = texts.unzip();
                turn_item!(FileChange(FileChangeItem), shared, {
                    file_name: file_name,
                    additions: None,
                    deletions: None,
                    diff_str: patch.or_else(|| previous.and_then(|p| p.diff_str.clone())),
                    old_str: old_str.or_else(|| previous.and_then(|p| p.old_str.clone())),
                    new_str: new_str.or_else(|| previous.and_then(|p| p.new_str.clone())),
                    changes: Some(changes),
                    error: error,
                })
            }
            Work::Tool { name, input, output, viewed_image_path } => {
                let previous = match previous {
                    Some(TurnItem::DynamicTool(previous)) => Some(previous),
                    _ => None,
                };
                turn_item!(DynamicTool(DynamicToolItem), shared, {
                    tool_name: Some(name),
                    viewed_image_path: viewed_image_path.or_else(|| previous.and_then(|p| p.viewed_image_path.clone())),
                    input: Some(input),
                    output: output.or_else(|| previous.and_then(|p| p.output.clone())),
                    output_omitted: None,
                })
            }
            Work::WebSearch { patterns, results } => turn_item!(WebSearch(WebSearchItem), shared, { patterns: Some(patterns), results: results }),
            Work::Reasoning { text, streaming } => turn_item!(Reasoning(ReasoningItem), shared, { text: text, streaming: streaming }),
            Work::Compaction { before_token_count } => turn_item!(Compaction(CompactionItem), shared, {
                driver: Some(ProviderDriverKind::from(self.driver.as_str())),
                summary: None,
                before_token_count: before_token_count,
                after_token_count: None,
            }),
            Work::Notice { message } => turn_item!(SystemNotice(SystemNoticeItem), shared, { message: message }),
        }
    }

    /// A plan the agent wrote. Records the plan artifact (`plan.updated`, kept
    /// `active` until implemented) and the turn item that shows it.
    pub fn plan(&mut self, native_id: &str, plan: Plan, completed: bool) {
        self.together(|turn| turn.write_plan(native_id, plan, completed));
    }

    fn write_plan(&mut self, native_id: &str, plan: Plan, completed: bool) {
        self.held.remove(native_id);
        let status = if completed { TurnItemStatus::Completed } else { TurnItemStatus::Running };
        let node_kind = match plan {
            Plan::Proposed { .. } => NodeKind::Plan,
            Plan::Todo { .. } => NodeKind::TodoList,
        };
        let node_id = self.node_in(native_id, node_kind, node_status(&status), self.native_item_id("node", native_id));
        let plan_id = PlanId::from(self.native_item_id("plan", native_id));
        let (thread_id, run_id) = (ThreadId::from(self.thread_id.as_str()), Some(self.run.id.clone()));
        let shared = self.shared(native_id, self.native_item_id("turn-item", native_id), &node_id, native_id, status);
        let (artifact, item) = match plan {
            Plan::Proposed { markdown } => {
                if completed
                    && let Ok(existing) = self.store.records::<PlanArtifact>(&self.thread_id)
                {
                    for old in existing {
                        if let PlanArtifact::ProposedPlan(old) = &*old
                            && old.id != plan_id
                            && matches!(old.status, PlanStatus::Active | PlanStatus::Draft)
                        {
                            let mut superseded = old.clone();
                            superseded.status = PlanStatus::Superseded;
                            self.emit(EventBody::PlanUpdated(Box::new(PlanArtifact::ProposedPlan(superseded))));
                        }
                    }
                }
                (
                    PlanArtifact::ProposedPlan(ProposedPlan {
                        id: plan_id.clone(), thread_id, run_id, node_id, status: if completed { PlanStatus::Active } else { PlanStatus::Draft }, detail_in_turn_item: None, markdown: markdown.clone(),
                    }),
                    turn_item!(ProposedPlan(ProposedPlanItem), shared, { plan_id: plan_id, markdown: markdown, streaming: !completed }),
                )
            }
            Plan::Todo { steps, explanation } => (
                PlanArtifact::TodoList(TodoList {
                    id: plan_id.clone(), thread_id, run_id, node_id, status: PlanStatus::Active, detail_in_turn_item: None,
                    steps: steps.clone(), explanation: explanation.clone(),
                }),
                turn_item!(TodoList(TodoListItem), shared, { plan_id: plan_id, steps: steps, explanation: explanation }),
            ),
        };
        self.emit(EventBody::PlanUpdated(Box::new(artifact)));
        self.items.insert(native_id.to_owned(), item.clone());
        self.emit(EventBody::TurnItemUpdated(item));
    }

    /// Whether a streaming update of `native_id` may go out now (else it
    /// waits for the next one, or the turn's end).
    pub fn may_flush(&mut self, native_id: &str, len: usize) -> bool {
        if self.last_flush.get(native_id).is_some_and(|t| t.elapsed() < flush_interval(len)) {
            return false;
        }
        self.last_flush.insert(native_id.to_owned(), Instant::now());
        true
    }

    /// A streaming tool or reasoning item (thinking text growing token by
    /// token): throttled like assistant text, `text_len` being its size.
    pub fn item_streaming(&mut self, native_id: &str, work: Work, text_len: usize) {
        if !self.may_flush(native_id, text_len) {
            self.held.insert(native_id.to_owned(), Held::Item(work));
            return;
        }
        self.held.remove(native_id);
        self.item(native_id, work, TurnItemStatus::Running);
    }

    /// One streamed delta of a reply's text: appended to `text`, then sent
    /// as the reply so far (throttled like any streaming update).
    pub fn assistant_delta(&mut self, native_id: &str, text: &mut String, delta: &str) {
        text.push_str(delta);
        if !self.may_flush(native_id, text.len()) {
            // Held back by the throttle: what is held grows by the delta. The
            // whole reply is copied once per flush, not once per token.
            match self.held.get_mut(native_id) {
                Some(Held::Assistant { text: held, .. }) if held.len() + delta.len() == text.len() => held.push_str(delta),
                _ => {
                    self.held.insert(native_id.to_owned(), Held::Assistant { text: text.clone(), phase: None });
                }
            }
            return;
        }
        self.held.remove(native_id);
        self.together(|turn| turn.write_assistant(native_id, text, true, None));
    }

    /// Assistant text. While streaming, updates are throttled; the final call
    /// (`streaming == false`) always goes out.
    pub fn assistant(&mut self, native_id: &str, text: &str, streaming: bool, phase: Option<&str>) {
        if streaming && !self.may_flush(native_id, text.len()) {
            self.held.insert(native_id.to_owned(), Held::Assistant { text: text.to_owned(), phase: phase.map(str::to_owned) });
            return;
        }
        self.held.remove(native_id);
        self.together(|turn| turn.write_assistant(native_id, text, streaming, phase));
    }

    /// The message an assistant item shows, as of `updated_at`.
    fn assistant_message(&self, id: MessageId, node_id: Option<NodeId>, text: String, streaming: bool, created_at: Timestamp, updated_at: Timestamp) -> ConversationMessage {
        ConversationMessage {
            notification: None,
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            workflow_script: None,
            id,
            thread_id: ThreadId::from(self.thread_id.as_str()),
            run_id: Some(self.run.id.clone()),
            node_id,
            role: MessageRole::Assistant,
            text,
            context: None,
            attachments: Vec::new(),
            streaming,
            created_at,
            updated_at,
            delegated_completion: None,
        }
    }

    fn write_assistant(&mut self, native_id: &str, text: &str, streaming: bool, phase: Option<&str>) {
        let status = if streaming { TurnItemStatus::Running } else { TurnItemStatus::Completed };
        let node_id = self.node_in(native_id, NodeKind::AssistantMessage, node_status(&status), self.native_item_id("node", native_id));
        let message_id = MessageId::from(self.native_item_id("message", native_id));
        let ts = now();
        let previous = match self.items.get(native_id) {
            Some(TurnItem::AssistantMessage(previous)) => Some(previous),
            _ => None,
        };
        let created_at = previous.and_then(|p| p.started_at.clone()).unwrap_or_else(|| ts.clone());
        let phase = phase.and_then(spelled).or_else(|| previous.and_then(|p| p.phase.clone()));
        // While a reply streams its turn item carries it. The message, which
        // holds the same text for whoever reads a conversation's messages,
        // is written once the reply is whole: each flush sends the text once.
        if !streaming {
            self.emit(EventBody::MessageUpdated(Box::new(self.assistant_message(message_id.clone(), Some(node_id.clone()), text.to_owned(), false, created_at, ts))));
        }
        let shared = self.shared(native_id, self.native_item_id("turn-item", native_id), &node_id, native_id, status);
        let item = turn_item!(AssistantMessage(AssistantMessageItem), shared, {
            phase: phase,
            message_id: message_id,
            text: text.to_owned(),
            text_omitted: None,
            attachments: None,
            streaming: streaming,
        });
        self.put_item(native_id, item);
    }

    /// The id of a new request for `native_request_id`, its node (waiting,
    /// under the item `native_item_id` it is about) and its item's shared fields.
    fn open(&mut self, native_request_id: &str, native_item_id: &str) -> (RuntimeRequestId, NodeId, Shared) {
        let request_id = format!(
            "runtime-request:provider:{}:provider-turn:{}:native-request:{}:{}",
            self.driver, enc(self.provider_turn_id().map_or("none", ProviderTurnId::as_str)), enc(native_request_id), crate::util::new_uuid()
        );
        let parent = self.nodes.get(native_item_id).map_or_else(|| self.root.clone(), |node| node.id.clone());
        let node_id = format!("node:runtime-request:{}", enc(&request_id));
        let id = RuntimeRequestId::from(request_id.as_str());
        let node_id = self.node_for(&request_id, NodeKind::ApprovalRequest, NodeStatus::Waiting, Some(parent), Some(id.clone()), node_id).id;
        let item_id = format!("turn-item:runtime-request:{}", enc(&request_id));
        let shared = self.shared(&request_id, item_id, &node_id, native_item_id, TurnItemStatus::Waiting);
        (id, node_id, shared)
    }

    /// Records the item of a request just opened, then the request itself, pending.
    fn request(&mut self, id: RuntimeRequestId, node_id: NodeId, item: TurnItem, native_item_id: &str, kind: RuntimeRequestKind, by_message: bool) -> String {
        self.items.insert(id.as_str().to_owned(), item.clone());
        self.emit(EventBody::TurnItemUpdated(item));
        let request = RuntimeRequest {
            id: id.clone(),
            node_id,
            provider_turn_id: self.provider_turn_id().cloned(),
            native_request_ref: Some(native_ref(&self.driver, native_item_id)),
            kind,
            status: RuntimeRequestStatus::Pending,
            response_capability: if by_message {
                ResponseCapability::Message
            } else {
                ResponseCapability::Live { provider_session_id: ProviderSessionId::from(self.session_id.as_str()), supports_feedback: None }
            },
            created_at: now(),
            resolved_at: None,
            decision: None,
            feedback: None,
            answers: None,
        };
        self.emit(EventBody::RuntimeRequestUpdated(Box::new(request.clone())));
        if !by_message {
            self.requests.insert(id.as_str().to_owned(), request);
        }
        id.as_str().to_owned()
    }

    /// Opens an approval request attached to a tool item. Returns its id.
    pub fn open_request(&mut self, native_request_id: &str, native_item_id: &str, kind: ProviderRequestKind, prompt: &str) -> String {
        self.open_request_with(native_request_id, native_item_id, kind, prompt, None)
    }

    /// [`Turn::open_request`] with the answers the agent offers, as the
    /// client shows them (labels, and a warning on one).
    pub fn open_request_with(&mut self, native_request_id: &str, native_item_id: &str, kind: ProviderRequestKind, prompt: &str, options: Option<Vec<ApprovalOption>>) -> String {
        let (id, node_id, shared) = self.open(native_request_id, native_item_id);
        let item = turn_item!(ApprovalRequest(ApprovalRequestItem), shared, {
            request_id: id.clone(),
            request_kind: kind.clone(),
            prompt: Some(prompt.to_owned()),
            reason: None,
            full_access_override: None,
            app_name: None,
            options: options,
        });
        self.request(id, node_id, item, native_item_id, request_kind(&kind), false)
    }

    /// A question the agent asks (Claude's `AskUserQuestion`), as a
    /// `user_input` request with a `user_input_request` item carrying its
    /// questions; `runtime-request.respond` with `answers` resolves it.
    pub fn open_user_input(&mut self, native_request_id: &str, native_item_id: &str, questions: Vec<UserInputQuestion>) -> String {
        self.user_input(native_request_id, native_item_id, questions, false)
    }

    pub fn open_message_question(&mut self, native_item_id: &str, questions: Vec<UserInputQuestion>) {
        let db = self.store.db().clone();
        let result = db.batch(|| -> Result<(), crate::store::StoreError> {
            let thread = self.store.app_thread(&self.thread_id)?;
            if thread.is_some_and(|thread| thread.deleted_at.is_none() && thread.provider_instance_id.as_str() == self.instance) {
                self.user_input(native_item_id, native_item_id, questions, true);
            }
            Ok(())
        });
        if let Err(error) = result {
            tracing::error!(thread = %self.thread_id, "could not publish the question: {error}");
        }
    }

    fn user_input(&mut self, native_request_id: &str, native_item_id: &str, questions: Vec<UserInputQuestion>, by_message: bool) -> String {
        let (id, node_id, shared) = self.open(native_request_id, native_item_id);
        let item = turn_item!(UserInputRequest(UserInputRequestItem), shared, {
            request_id: id.clone(),
            questions: questions,
            question_answer: None,
            response_mode: by_message.then_some(ResponseMode::Message),
        });
        self.request(id, node_id, item, native_item_id, RuntimeRequestKind::UserInput, by_message)
    }

    pub fn resolve_request(&mut self, request_id: &str, decision: &str, answers: Option<&Answers>) {
        self.settle_live_request(request_id, decision, answers, false);
    }

    pub fn cancel_request(&mut self, request_id: &str) {
        self.settle_live_request(request_id, "cancel", None, true);
    }

    fn settle_live_request(&mut self, request_id: &str, decision: &str, answers: Option<&Answers>, cancelled: bool) {
        let Some(mut request) = self.requests.remove(request_id) else { return };
        request.status = if cancelled { RuntimeRequestStatus::Cancelled } else { RuntimeRequestStatus::Resolved };
        request.decision = spelled(decision);
        if let Some(answers) = answers {
            request.answers = Some(answers.clone());
        }
        request.resolved_at = Some(now());
        self.emit(EventBody::RuntimeRequestUpdated(Box::new(request)));
        if self.nodes.contains_key(request_id) {
            let status = if cancelled { NodeStatus::Cancelled } else { NodeStatus::Completed };
            self.node_for(request_id, NodeKind::ApprovalRequest, status, None, None, String::new());
        }
        if let Some(mut item) = self.items.remove(request_id) {
            let status = if cancelled { TurnItemStatus::Cancelled } else { TurnItemStatus::Completed };
            end_item(&mut item, status, &now());
            self.items.insert(request_id.to_owned(), item.clone());
            self.emit(EventBody::TurnItemUpdated(item));
        }
    }

    /// Settles the turn. Completed runs end in `waiting`; run finalization
    /// captures the checkpoint and then marks them `completed`.
    pub fn complete(&mut self, end: TurnEnd, error: Option<&str>) {
        let closed = if end == TurnEnd::Completed { TurnItemStatus::Completed } else { TurnItemStatus::Interrupted };
        // What the throttle held back goes out as the item's final state.
        for (native_id, held) in std::mem::take(&mut self.held) {
            match held {
                Held::Assistant { text, phase } => self.assistant(&native_id, &text, false, phase.as_deref()),
                Held::Item(mut work) => {
                    if let Work::Reasoning { streaming, .. } = &mut work {
                        *streaming = false;
                    }
                    self.item(&native_id, work, closed.clone());
                }
            }
        }
        let ts = now();
        for (_, mut request) in std::mem::take(&mut self.requests) {
            request.status = RuntimeRequestStatus::Cancelled;
            request.resolved_at = Some(ts.clone());
            self.emit(EventBody::RuntimeRequestUpdated(Box::new(request)));
        }
        // Close items the provider left open.
        let open: Vec<String> = self
            .items
            .iter()
            .filter(|(_, item)| {
                matches!(item.status(), TurnItemStatus::Running | TurnItemStatus::Waiting | TurnItemStatus::Pending)
                    && !matches!(item, TurnItem::UserInputRequest(question) if question.response_mode == Some(ResponseMode::Message))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in open {
            if let Some(mut item) = self.items.remove(&key) {
                end_item(&mut item, closed.clone(), &ts);
                // The message an assistant item shows stops streaming too
                // (otherwise a stopped reply spins for ever).
                if let TurnItem::AssistantMessage(shown) = &item {
                    let created_at = shown.started_at.clone().unwrap_or_else(|| ts.clone());
                    let message = self.assistant_message(shown.message_id.clone(), shown.node_id.clone(), shown.text.clone(), false, created_at, ts.clone());
                    self.emit(EventBody::MessageUpdated(Box::new(message)));
                }
                self.emit(EventBody::TurnItemUpdated(item));
            }
        }
        if let Some(mut turn) = self.provider_turn.clone() {
            turn.status = match end {
                TurnEnd::Completed => ProviderTurnStatus::Completed,
                TurnEnd::Interrupted => ProviderTurnStatus::Interrupted,
                TurnEnd::Failed => ProviderTurnStatus::Failed,
            };
            turn.completed_at = Some(ts.clone());
            self.emit(EventBody::ProviderTurnUpdated(Box::new(turn)));
        }
        let provider_turn_id = self.provider_turn_id().cloned();
        if let Some(attempt) = &self.run.active_attempt_id {
            self.update(attempt.as_str(), |attempt: &mut RunAttempt| {
                attempt.status = match end {
                    TurnEnd::Completed => RunAttemptStatus::Completed,
                    TurnEnd::Interrupted => RunAttemptStatus::Interrupted,
                    TurnEnd::Failed => RunAttemptStatus::Failed,
                };
                attempt.completed_at = Some(ts.clone());
                if let Some(id) = provider_turn_id {
                    attempt.provider_turn_id = Some(id);
                }
            });
        }
        // The conversation now continues from this turn, not from a rollback point.
        self.provider_thread.native_conversation_head_ref = None;
        self.thread_status(ProviderThreadStatus::Idle, None);
        if end == TurnEnd::Failed {
            let error = error.unwrap_or("The provider turn failed.");
            match self.usage_limited.clone() {
                Some(reset_at) => crate::orchestration::effects::fail_run_with(&self.store, &self.thread_id, &self.run_id(), error, ProviderFailureClass::UsageLimit, reset_at),
                None => crate::orchestration::effects::fail_run(&self.store, &self.thread_id, &self.run_id(), error, ProviderFailureClass::ProviderError),
            }
            return;
        }
        let settle = |run: &mut Run| {
            run.status = if end == TurnEnd::Completed { RunStatus::Waiting } else { RunStatus::Interrupted };
            if end != TurnEnd::Completed {
                run.completed_at = Some(ts.clone());
            }
        };
        if self.update(self.run.id.as_str(), settle).is_none() {
            let mut run = self.run.clone();
            settle(&mut run);
            self.emit(EventBody::RunUpdated(Box::new(run)));
        }
        self.update(self.root.as_str(), |node: &mut ExecutionNode| {
            node.status = if end == TurnEnd::Completed { NodeStatus::Waiting } else { NodeStatus::Interrupted };
            if end != TurnEnd::Completed {
                node.completed_at = Some(ts.clone());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::commands::Orchestrator;
    use serde_json::json;

    /// A turn of thread `t1`'s first run, on `instance`.
    async fn turn_on(instance: &str) -> (Store, Turn) {
        let store = Store::new(crate::db::Db::open_in_memory().expect("db"));
        store
            .create_project(crate::wire::shell::ProjectShell {
                id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: std::env::temp_dir().to_string_lossy().into(),
                repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
                favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
                created_at: "t".into(), updated_at: "t".into(),
            }, None)
            .expect("project");
        let (orchestrator, _effects) = Orchestrator::new(store.clone());
        orchestrator.launch_thread(crate::test_records::launch("c1", "t1", "p", instance, "hello")).await.expect("launch");
        let run = store.records::<Run>("t1").expect("runs").first().expect("a run").id.clone();
        let turn = Turn::begin(store.clone(), "t1", run.as_str()).expect("the run and its provider thread are there");
        (store, turn)
    }

    /// The thread's stored records of the collection `kind`, as the JSON clients are sent.
    fn of_kind(store: &Store, kind: &str) -> Vec<Value> {
        crate::test_records::stored_of_kind(store, "t1", domovoi_model::typed::RecordKind::of_collection(kind).expect("a collection"))
    }

    #[tokio::test]
    async fn a_turn_records_its_work_and_leaves_the_run_waiting() {
        for instance in ["claudeAgent", "codex"] {
            let (store, mut turn) = turn_on(instance).await;
            let _ = store.thread_shell("t1").expect("shell"); // cached, as with a client watching
            turn.session(ProviderSessionStatus::Ready, "/work", "m", None);
            turn.thread_status(ProviderThreadStatus::Active, Some("native-1"));
            turn.turn_started("native-turn-1");
            assert_eq!(of_kind(&store, "providerTurns")[0]["status"], "running", "{instance}");
            assert_eq!(of_kind(&store, "runs")[0]["status"], "running");
            turn.token_usage(WindowUse { used_tokens: 10, max_tokens: None, input_tokens: 6, cached_input_tokens: 0, output_tokens: 4, reasoning_output_tokens: 0 });
            turn.assistant("a1", "Hel", true, Some("commentary"));
            turn.item("r1", Work::Reasoning { text: "thinking".into(), streaming: true }, TurnItemStatus::Running);
            turn.item("c1", Work::Command { input: "ls".into(), output: None, exit_code: None }, TurnItemStatus::Running);
            turn.item("c1", Work::Command { input: "ls".into(), output: Some("a.txt".into()), exit_code: Some(0) }, TurnItemStatus::Completed);
            // What a later update does not say stays as it was.
            turn.item("c1", Work::Command { input: "ls".into(), output: None, exit_code: None }, TurnItemStatus::Completed);
            let changes = vec![FileChangeDetail { operation: "update".into(), path: "src/a.rs".into(), old_path: None, file_type: None, mime_type: None }];
            turn.item("f1", Work::FileChange { file_name: "a.rs".into(), changes, texts: None, patch: Some("@@ -1 +1 @@".into()), error: None }, TurnItemStatus::Completed);
            turn.plan("p1", Plan::Todo { steps: vec![PlanStep { id: "step-1".into(), text: "Look".into(), status: domovoi_model::typed::PlanStepStatus::Running, duration_anchor_at: None, duration_ms: None }], explanation: None }, true);
            let approval = turn.open_request("req-1", "c1", ProviderRequestKind::Command, "Run ls");
            let asked: Vec<UserInputQuestion> = serde_json::from_value(json!([
                { "id": "db", "header": "Database", "question": "Which one?", "options": [{ "label": "Postgres", "description": "Relational" }], "multiSelect": false },
            ]))
            .expect("the question");
            let question = turn.open_user_input("req-2", "ask-1", asked);
            turn.resolve_request(&approval, "accept", None);
            turn.complete(TurnEnd::Completed, None);

            let items = of_kind(&store, "turnItems");
            let item = |item_type: &str| items.iter().find(|i| i["type"] == item_type).unwrap_or_else(|| panic!("no {item_type} item")).clone();
            assert_eq!((item("assistant_message")["text"].as_str(), item("assistant_message")["streaming"].as_bool()), (Some("Hel"), Some(false)));
            assert_eq!(item("assistant_message")["phase"], "commentary");
            assert_eq!(item("reasoning")["streaming"], false, "an item left open is closed with the turn");
            assert_eq!((item("command_execution")["output"].as_str(), item("command_execution")["exitCode"].as_i64()), (Some("a.txt"), Some(0)));
            assert_eq!(item("command_execution")["completionWake"], "always");
            assert_eq!((item("file_change")["diffStr"].as_str(), item("file_change").get("diff")), (Some("@@ -1 +1 @@"), None), "a patch is kept where the schema has it");
            assert_eq!(item("user_input_request")["questions"][0]["options"][0]["label"], "Postgres");
            assert_eq!(item("approval_request")["status"], "completed");
            let requests = of_kind(&store, "runtimeRequests");
            let request = |id: &str| requests.iter().find(|r| r["id"] == id).expect("request").clone();
            assert_eq!((request(&approval)["status"].as_str(), request(&approval)["decision"].as_str()), (Some("resolved"), Some("accept")));
            assert_eq!(request(&question)["status"], "cancelled", "a request nobody answered ends with the turn");
            assert_eq!(of_kind(&store, "runs")[0]["status"], "waiting");
            assert_eq!(of_kind(&store, "providerTurns")[0]["tokenUsage"]["usedTokens"], 10);
            let provider_thread = &of_kind(&store, "providerThreads")[0];
            assert_eq!((provider_thread["status"].as_str(), provider_thread["contextUsage"]["usedTokens"].as_u64()), (Some("idle"), Some(10)));
            assert!(provider_thread["contextUsage"].get("maxTokens").is_none(), "a window of unknown size is left out");
            assert!(of_kind(&store, "messages").iter().all(|m| m["streaming"] == false));
            let cached = store.thread_shell("t1").expect("shell").expect("thread");
            assert_eq!(*cached, crate::orchestration::projection::thread_shell(&store, "t1").expect("shell").expect("thread"), "{instance}");
        }
    }
}
