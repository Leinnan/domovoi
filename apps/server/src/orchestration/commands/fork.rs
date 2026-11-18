//! `thread.fork` and `thread.merge_back`: context goes from one thread to
//! another as a pending transfer, which the target's next message takes up
//! (`message`).

use super::{DispatchResult, Orchestrator, Refusal, now};
use crate::orchestration::context;
use crate::orchestration::events::enc;
use domovoi_model::typed::command::{ThreadFork, ThreadMergeBack};
use domovoi_model::typed::{
    Actor, AppThread, CommandId, ContextTransfer, ContextTransferStatus, ContextTransferType, CreationSource, EventBody, ForkSource, ForkSourcePoint,
    NativeRefStrength, ParentRelationship, ThreadChange, ThreadId, ThreadLineage, Timestamp,
};

/// A fork, or the reverse: a fork's work going back into its parent.
pub(super) struct Fork<'a> {
    command_id: &'a CommandId,
    source_thread_id: &'a ThreadId,
    target_thread_id: &'a ThreadId,
    source_point: &'a ForkSourcePoint,
    created_by: &'a Actor,
    creation_source: &'a CreationSource,
    created_at: Option<&'a Timestamp>,
    /// What the new thread is, for a fork; `None` for a merge back, which creates none.
    new_thread: Option<NewThread<'a>>,
}

struct NewThread<'a> {
    title: Option<&'a str>,
    side_chat: bool,
}

impl<'a> Fork<'a> {
    pub(super) fn new_thread(c: &'a ThreadFork) -> Self {
        Fork {
            command_id: &c.command_id,
            source_thread_id: &c.source_thread_id,
            target_thread_id: &c.target_thread_id,
            source_point: &c.source_point,
            created_by: &c.created_by,
            creation_source: &c.creation_source,
            created_at: c.created_at.as_ref(),
            new_thread: Some(NewThread { title: c.title.as_deref(), side_chat: c.side_chat == Some(true) }),
        }
    }

    pub(super) fn merge_back(c: &'a ThreadMergeBack) -> Self {
        Fork {
            command_id: &c.command_id,
            source_thread_id: &c.source_thread_id,
            target_thread_id: &c.target_thread_id,
            source_point: &c.source_point,
            created_by: &c.created_by,
            creation_source: &c.creation_source,
            created_at: c.created_at.as_ref(),
            new_thread: None,
        }
    }
}

impl Orchestrator {
    /// `thread.fork` (or `thread.merge_back`, the reverse: a fork's work back
    /// into its parent): the target gets a pending context transfer that its
    /// next message takes up. A fork is a new thread inheriting the source's
    /// settings and workspace (t3code ThreadForkService).
    pub(super) fn fork(&self, fork: Fork<'_>) -> Result<DispatchResult, Refusal> {
        let (source_id, target_id) = (fork.source_thread_id, fork.target_thread_id);
        let source = self.thread(source_id)?;
        let outline = self.store.thread_outline(source_id.as_str())?;
        let run = context::source_run(&outline.runs, fork.source_point)?;
        let point = context::source_point(source_id, &outline.provider_threads, &outline.attempts, &outline.provider_turns, run);
        let at = fork.created_at.cloned().unwrap_or_else(now);
        let origin = Self::origin(fork.command_id, Some(&run.provider_instance_id));
        match &fork.new_thread {
            None => {
                self.thread(target_id)?;
                if source.lineage.parent_thread_id.as_ref() != Some(target_id) {
                    return Err(Refusal::NotAForkOfTarget);
                }
            }
            Some(new) => {
                if self.store.app_thread(target_id.as_str())?.is_some() {
                    return Err(Refusal::ThreadExists(target_id.clone()));
                }
                let created = AppThread {
                    id: target_id.clone(),
                    title: new.title.map_or_else(|| format!("{} fork", source.title), str::to_owned),
                    created_by: fork.created_by.clone(),
                    creation_source: fork.creation_source.clone(),
                    lineage: ThreadLineage {
                        parent_thread_id: Some(source_id.clone()),
                        relationship_to_parent: Some(ParentRelationship::Fork),
                        root_thread_id: source.lineage.root_thread_id.clone(),
                        side_chat: new.side_chat.then_some(true),
                    },
                    forked_from: Some(ForkSource::Run { thread_id: source_id.clone(), run_id: run.id.clone() }),
                    created_at: at.clone(),
                    updated_at: at.clone(),
                    // What the source was doing, and how it was shown, is not the fork's.
                    title_regeneration: Some(None),
                    last_title_regeneration: Some(None),
                    pinned_at: Some(None),
                    pin_order_key: Some(None),
                    active_order_key: Some(None),
                    archived_at: None,
                    settled_override: Some(None),
                    settled_at: Some(None),
                    unsettled_at: Some(None),
                    snoozed_until: Some(None),
                    snoozed_at: Some(None),
                    last_snooze_wake_at: Some(None),
                    auto_settle_disabled_at: Some(None),
                    last_visited_at: Some(None),
                    opens: Some(None),
                    deleted_at: None,
                    active_provider_thread_id: None,
                    // Its settings and workspace are.
                    ..source
                };
                self.store.append_event(target_id.as_str(), EventBody::Thread { change: ThreadChange::Created, thread: Box::new(created) }, &origin)?;
            }
        }
        let merge_back = fork.new_thread.is_none();
        let strong = point.provider_thread_ref.as_ref().is_some_and(|native| native.strength == NativeRefStrength::Strong);
        let name = if merge_back { "merge-back" } else { "fork" };
        let mut id = format!("context-transfer:{name}:{}:{}", enc(source_id.as_str()), enc(target_id.as_str()));
        if merge_back {
            // A fork can be merged back more than once.
            id = format!("{id}:{}", crate::util::new_uuid());
        }
        let transfer = ContextTransfer {
            id: id.into(),
            kind: if merge_back { ContextTransferType::MergeBack } else { ContextTransferType::Fork },
            source_thread_id: source_id.clone(),
            target_thread_id: target_id.clone(),
            source_point: point,
            // What the fork inherited is already in the parent: a merge back hands over only its own runs.
            base_point: None,
            source_provider_instance_id: Some(run.provider_instance_id.clone()),
            source_token_usage: if merge_back {
                None
            } else {
                context::provider_turn_of(&outline.attempts, &outline.provider_turns, run).and_then(|turn| turn.token_usage.clone())
            },
            target_provider_instance_id: None,
            target_run_id: None,
            status: ContextTransferStatus::Pending,
            resolution: None,
            created_by: fork.created_by.clone(),
            error: (!strong && !merge_back).then(|| "Source provider thread does not expose a strong native thread ref.".to_owned()),
            created_at: at.clone(),
            updated_at: at,
            consumed_at: None,
        };
        let sequence = self.store.append_event(target_id.as_str(), EventBody::ContextTransferCreated(Box::new(transfer)), &origin)?.sequence;
        self.answer(Some(sequence))
    }
}
