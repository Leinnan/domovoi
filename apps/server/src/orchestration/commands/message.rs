//! `message.dispatch`: a message starts a run, queues one behind the run
//! going, restarts that run, or joins its turn (a steer). A run that starts on
//! a provider thread which has not seen the conversation gets it handed over.

use super::queue::placeholder_provider_thread;
use super::{DispatchResult, Effect, Orchestrator, Refusal, attempt_id, can_native_fork, can_steer, driver_for_instance, is_active, now, root_node_id, run_id, scope_id};
use crate::adapters::turn::{Shared, turn_item};
use crate::orchestration::context;
use crate::orchestration::events::{Outline, enc};
use crate::store::Store;
use std::collections::BTreeSet;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::command::MessageDispatch;
use domovoi_model::typed::{
    Actor, AppThread, ChatAttachment, CheckpointScope, CheckpointScopeKind, CommandId, ContextHandoff, ContextTransfer, ContextTransferId,
    ContextTransferResolution, ContextTransferStatus, ContextTransferType, ConversationMessage, CreationSource, DispatchMode, EventBody,
    ExecutionNode, HandoffStatus, HandoffStrategy, InputIntent, InteractionMode, MessageId, MessageRole, NodeKind, NodeStatus, PlanArtifact, PlanStatus,
    ProviderInstanceId, ProviderThread, ProviderThreadFork, ProviderThreadId, ProviderTurn, ProviderTurnStatus, Run, RunAttempt, RunAttemptReason,
    RunAttemptStatus, RunId, RunOrdinalRange, RunStatus, ScheduledTaskId, Thread, ThreadChange, ThreadId, Timestamp, TitleRegeneration, TurnItem, TurnItemStatus,
};

/// A message as it reaches the agent: what the user message and its turn item share.
pub(super) struct Steered<'a> {
    pub message_id: &'a MessageId,
    pub text: &'a str,
    pub attachments: &'a [ChatAttachment],
    pub created_by: &'a Actor,
    pub creation_source: &'a CreationSource,
    /// How it reached the agent: starting a turn, from the queue, or into a running turn.
    pub intent: InputIntent,
    /// The scheduled task whose occurrence this is.
    pub scheduled_task_id: Option<&'a ScheduledTaskId>,
    /// The run starts a new provider session (`freshContext`).
    pub fresh_context: bool,
}

/// The user's message as a record of the conversation, belonging to run `run_id`.
fn user_message(thread_id: &ThreadId, run_id: &str, node_id: &str, sent: &Steered<'_>, at: &Timestamp) -> ConversationMessage {
    ConversationMessage {
        notification: None,
        created_by: sent.created_by.clone(),
        creation_source: sent.creation_source.clone(),
        scheduled_task_id: sent.scheduled_task_id.cloned(),
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        id: sent.message_id.clone(),
        thread_id: thread_id.clone(),
        run_id: Some(run_id.into()),
        node_id: Some(node_id.into()),
        role: MessageRole::User,
        text: sent.text.to_owned(),
        context: None,
        attachments: sent.attachments.to_vec(),
        streaming: false,
        created_at: at.clone(),
        updated_at: at.clone(),
        delegated_completion: None,
    }
}

/// The timeline entry that shows a user's message: the first item of its
/// run, unless the caller places it elsewhere.
pub(super) fn user_turn_item(
    thread_id: &str,
    run_id: &str,
    node_id: &str,
    provider_thread_id: Option<&ProviderThreadId>,
    run_ordinal: u64,
    sent: Steered<'_>,
    at: &Timestamp,
) -> TurnItem {
    let shared = Shared {
        id: format!("turn-item:message:{}", enc(sent.message_id.as_str())).into(),
        thread_id: thread_id.into(),
        run_id: Some(run_id.into()),
        node_id: Some(node_id.into()),
        provider_thread_id: provider_thread_id.cloned(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: run_ordinal * 1_000_000 + 1,
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(at.clone()),
        completed_at: Some(at.clone()),
        updated_at: at.clone(),
    };
    turn_item!(UserMessage(UserMessageItem), shared, {
        created_by: sent.created_by.clone(),
        creation_source: sent.creation_source.clone(),
        message_id: sent.message_id.clone(),
        scheduled_task_id: sent.scheduled_task_id.cloned(),
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        fresh_context: sent.fresh_context.then_some(true),
        input_intent: sent.intent,
        text: sent.text.to_owned(),
        text_omitted: None,
        context: None,
        attachments: sent.attachments.to_vec(),
        model_switch: None,
    })
}

/// The ordinal in a run id `run:thread:<t>:ordinal:<n>`.
fn run_ordinal_of(run_id: &str) -> u64 {
    run_id.rsplit(':').next().and_then(|n| n.parse().ok()).unwrap_or(1)
}

/// The provider turn `run` is in, if it is running one.
fn running_provider_turn<'a>(outline: &'a Outline, run: &Run) -> Option<&'a ProviderTurn> {
    let attempt = outline.attempts.iter().find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref());
    let of_attempt = attempt.and_then(|attempt| attempt.provider_turn_id.as_ref());
    outline
        .provider_turns
        .iter()
        .map(|turn| &**turn)
        .find(|turn| turn.status == ProviderTurnStatus::Running && (turn.run_attempt_id == run.active_attempt_id || of_attempt == Some(&turn.id)))
}

/// The run `run_id` if a provider turn of it is running and its driver can
/// take more input mid-turn.
pub(super) fn steerable_run(outline: &Outline, run_id: &RunId) -> Option<Run> {
    let run = outline.runs.iter().find(|run| run.id == *run_id && run.status == RunStatus::Running)?;
    let driver = &driver_for_instance(run.provider_instance_id.as_str());
    (can_steer(driver) && running_provider_turn(outline, run).is_some()).then(|| (**run).clone())
}

/// What a handoff is made for: the run that starts with it, on which provider
/// thread and instance, and the transfer it carries out, if any.
struct HandoffTarget<'a> {
    thread_id: &'a ThreadId,
    run_id: &'a str,
    provider_thread_id: &'a ProviderThreadId,
    instance: &'a ProviderInstanceId,
    transfer: Option<&'a ContextTransferId>,
    at: &'a Timestamp,
}

/// A ready handoff of runs `from..=to` of `source` for the target's run, and
/// the `handoff` turn item that shows it; `None` when there is nothing to say.
/// Where the conversation the provider sees begins (t3code
/// `contextBoundaryOrdinal`): the latest started fresh-context run that was
/// not a scheduled task's side conversation. Runs before it never reach a
/// provider again. 0 when there is none.
pub(super) fn context_boundary(store: &Store, thread_id: &str, outline: &Outline) -> u64 {
    outline
        .runs
        .iter()
        .filter(|run| run.fresh_context == Some(true) && run.provider_thread_id.is_some() && !matches!(run.status, RunStatus::Queued | RunStatus::Cancelled))
        .filter(|run| !scheduled_fresh_run(store, thread_id, run))
        .map(|run| run.ordinal)
        .max()
        .unwrap_or(0)
}

/// A scheduled task's fresh-session occurrence: a side conversation, which
/// the thread's own conversation continues past (t3code `isScheduledFreshRun`).
fn scheduled_fresh_run(store: &Store, thread_id: &str, run: &Run) -> bool {
    run.fresh_context == Some(true)
        && store.record::<ConversationMessage>(thread_id, run.user_message_id.as_str()).ok().flatten().is_some_and(|m| m.scheduled_task_id.is_some())
}

/// The provider thread a message continues on `instance` (t3code
/// `conversationProviderThread`): the thread's active one, unless a scheduled
/// task's fresh-session run started it; then the latest one of the thread's
/// own conversation since the boundary.
pub(super) fn conversation_provider_thread(store: &Store, thread_id: &str, outline: &Outline, thread: &AppThread, instance: &ProviderInstanceId) -> Option<ProviderThread> {
    let active = outline.provider_threads.iter().find(|pt| thread.active_provider_thread_id.as_ref() == Some(&pt.id) && pt.provider_instance_id == *instance)?;
    // A session works in one folder: a thread moved since (into a worktree)
    // starts a new one there, which the conversation is handed to.
    let folder = thread.worktree_path.clone().or_else(|| store.get_project(thread.project_id.as_str()).ok().flatten().map(|p| p.workspace_root));
    let worked_in = outline.checkpoint_scopes.iter().filter(|scope| scope.provider_thread_id.as_ref() == Some(&active.id)).max_by_key(|scope| scope.created_at.as_str()).map(|scope| scope.cwd.clone());
    if let (Some(now), Some(then)) = (&folder, &worked_in)
        && now != then
    {
        return None;
    }
    let started_by_scheduled = |pt: &ProviderThread| {
        outline.runs.iter().any(|run| Some(run.ordinal) == pt.first_run_ordinal && run.provider_thread_id.as_ref() == Some(&pt.id) && scheduled_fresh_run(store, thread_id, run))
    };
    if !started_by_scheduled(active) {
        return Some((**active).clone());
    }
    let boundary = context_boundary(store, thread_id, outline);
    let own = outline
        .provider_threads
        .iter()
        .filter(|pt| pt.provider_instance_id == *instance && pt.owner_node_id.is_none() && pt.last_run_ordinal.is_some_and(|last| last >= boundary))
        .filter(|pt| !started_by_scheduled(pt))
        .max_by_key(|pt| pt.last_run_ordinal);
    Some((**own.unwrap_or(active)).clone())
}

/// A new provider thread picks up this thread's conversation: the runs
/// before `ordinal` since the latest fresh start, as a handoff for run `run_id`.
#[expect(clippy::too_many_arguments, reason = "the run and provider thread a handoff is made for, each named")]
pub(super) fn catch_up(
    store: &Store,
    thread_id: &ThreadId,
    outline: &Outline,
    run_id: &str,
    ordinal: u64,
    provider_thread_id: &ProviderThreadId,
    instance: &ProviderInstanceId,
    at: &Timestamp,
) -> Result<Option<(ContextHandoff, TurnItem)>, crate::store::StoreError> {
    let tid = thread_id.as_str();
    let boundary = context_boundary(store, tid, outline);
    if !outline.runs.iter().any(|run| run.ordinal < ordinal && run.ordinal >= boundary && context::forkable(&run.status)) {
        return Ok(None);
    }
    let target = HandoffTarget { thread_id, run_id, provider_thread_id, instance, transfer: None, at };
    let why = "Earlier turns of this conversation ran with another agent or session.";
    Ok(store.load_thread(tid)?.and_then(|whole| handoff_for(&whole, boundary.max(1), ordinal - 1, &target, HandoffStrategy::FullThreadSummary, why)))
}

fn handoff_for(source: &Thread, from: u64, to: u64, target: &HandoffTarget<'_>, strategy: HandoffStrategy, why: &str) -> Option<(ContextHandoff, TurnItem)> {
    let (text, covered_from, covered_to) = context::transcript(source, from, to)?;
    let covered = || source.runs.iter().filter(|run| run.ordinal >= covered_from && run.ordinal <= covered_to);
    let from_provider_threads: BTreeSet<ProviderThreadId> = covered().filter_map(|run| run.provider_thread_id.clone()).collect();
    let from_instances: BTreeSet<ProviderInstanceId> = covered().map(|run| run.provider_instance_id.clone()).collect();
    let from_provider_threads: Vec<ProviderThreadId> = from_provider_threads.into_iter().collect();
    let (run_id, at) = (target.run_id, target.at);
    let id = format!("context-handoff:thread:{}:run:{}", enc(target.thread_id.as_str()), enc(run_id));
    let handoff = ContextHandoff {
        id: id.as_str().into(),
        transfer_id: Some(target.transfer.cloned()),
        thread_id: target.thread_id.clone(),
        target_run_id: run_id.into(),
        from_provider_thread_ids: from_provider_threads.clone(),
        to_provider_thread_id: target.provider_thread_id.clone(),
        covered_run_ordinals: RunOrdinalRange { from: covered_from.max(1), to: covered_to.max(1) },
        strategy,
        status: HandoffStatus::Ready,
        summary_message_id: None,
        summary_text: context::preamble(&text, why),
        history: None,
        delivery: None,
        detail_in_turn_item: None,
        created_by_provider_instance_id: None,
        created_at: at.clone(),
        updated_at: at.clone(),
    };
    let shared = Shared {
        id: format!("turn-item:context-handoff:{}", enc(&id)).into(),
        thread_id: target.thread_id.clone(),
        run_id: Some(run_id.into()),
        node_id: Some(root_node_id(run_id).into()),
        provider_thread_id: Some(target.provider_thread_id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: run_ordinal_of(run_id) * 1_000_000,
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(at.clone()),
        completed_at: Some(at.clone()),
        updated_at: at.clone(),
    };
    let item = turn_item!(Handoff(HandoffItem), shared, {
        context_handoff_id: id.as_str().into(),
        from_provider_thread_ids: from_provider_threads,
        to_provider_thread_id: target.provider_thread_id.clone(),
        from_provider_instance_ids: from_instances.into_iter().collect(),
        to_provider_instance_id: target.instance.clone(),
        from_model_selections: None,
        to_model: None,
        strategy: handoff.strategy.clone(),
        summary: None,
    });
    Some((handoff, item))
}

fn plan_status(plan: &PlanArtifact) -> &PlanStatus {
    match plan {
        PlanArtifact::ProposedPlan(plan) => &plan.status,
        PlanArtifact::TodoList(list) => &list.status,
    }
}

fn completed(mut plan: PlanArtifact) -> PlanArtifact {
    match &mut plan {
        PlanArtifact::ProposedPlan(plan) => plan.status = PlanStatus::Completed,
        PlanArtifact::TodoList(list) => list.status = PlanStatus::Completed,
    }
    plan
}

impl Orchestrator {
    pub(super) fn message_dispatch(&self, c: &MessageDispatch) -> Result<DispatchResult, Refusal> {
        self.message_dispatch_inner(c, false)
    }

    pub(super) fn message_dispatch_inner(&self, c: &MessageDispatch, answering_question: bool) -> Result<DispatchResult, Refusal> {
        let (thread_id, command_id) = (&c.thread_id, &c.command_id);
        let tid = thread_id.as_str();
        let mut thread = self.thread(thread_id)?;
        let outline = self.store.thread_outline(tid)?;
        let runs = &outline.runs;
        let at = now();
        let mut last = None;
        // Every event of the command is written on behalf of the provider the
        // thread had when the command arrived.
        let origin = Self::origin(command_id, Some(&thread.provider_instance_id));
        if !answering_question {
            self.close_message_questions(thread_id, &outline.runtime_requests, &origin)?;
        }
        let mut emit = |body: EventBody| -> Result<(), Refusal> {
            last = Some(self.store.append_event(tid, body, &origin)?.sequence);
            Ok(())
        };
        let thread_event = |change: ThreadChange, thread: &AppThread| EventBody::Thread { change, thread: Box::new(thread.clone()) };

        // "Implement this plan": the proposal must still be active; it is
        // completed by this message (t3code Orchestrator).
        let source_plan = match &c.source_plan_ref {
            None => None,
            Some(named) => {
                let plan = self
                    .store
                    .record::<PlanArtifact>(named.thread_id.as_str(), named.plan_id.as_str())?
                    .ok_or_else(|| Refusal::NoSuchPlan { plan: named.plan_id.clone(), thread: named.thread_id.clone() })?;
                if *plan_status(&plan) != PlanStatus::Active {
                    return Err(Refusal::PlanNotActive(named.plan_id.clone()));
                }
                Some((plan, named))
            }
        };
        // A fresh-context message starts a new provider session (and is never steered into a running one).
        let fresh = c.fresh_context == Some(true);
        // Steering: the message joins the running turn (t3code dispatchSteerIntoRun).
        let sent = |intent: InputIntent| Steered {
            message_id: &c.message_id,
            text: &c.text,
            attachments: &c.attachments,
            created_by: &c.created_by,
            creation_source: &c.creation_source,
            intent,
            scheduled_task_id: c.scheduled_task_id.as_ref(),
            fresh_context: fresh,
        };
        if let (DispatchMode::SteerActive { target_run_id }, None, false) = (&c.dispatch_mode, &source_plan, fresh)
            && let Some(target) = steerable_run(&outline, target_run_id)
        {
            return self.steer_into(command_id, thread_id, sent(InputIntent::Steer), &outline, &target);
        }
        let restart_target = match &c.dispatch_mode {
            DispatchMode::RestartActive { target_run_id } if runs.iter().any(|run| run.id == *target_run_id && is_active(&run.status)) => Some(target_run_id),
            _ => None,
        };
        // A message the user sends releases a queue held by a stop.
        let from_user = c.created_by == Actor::User && matches!(c.creation_source, CreationSource::Web | CreationSource::Mobile);
        if from_user {
            for held in runs.iter().filter(|run| run.status == RunStatus::Queued && run.queue_held == Some(true)) {
                let mut run = (**held).clone();
                run.queue_held = Some(false);
                emit(EventBody::RunUpdated(Box::new(run)))?;
            }
        }
        if let Some((plan, named)) = &source_plan {
            let plan = EventBody::PlanUpdated(Box::new(completed(plan.clone())));
            if named.thread_id == *thread_id {
                emit(plan)?;
            } else {
                self.store.append_event(named.thread_id.as_str(), plan, &origin)?;
            }
            // Implementing is not planning: out of plan mode before the run
            // starts, in this same command. A client's own mode change sent
            // beside it can arrive after the run has read the thread.
            if thread.interaction_mode == InteractionMode::Plan {
                thread.interaction_mode = InteractionMode::Default;
                thread.updated_at = at.clone();
                emit(thread_event(ThreadChange::InteractionModeUpdated, &thread))?;
                self.send_effect(Effect::RuntimeModeChanged { thread_id: tid.to_owned() });
            }
        }

        if thread.last_visited_at.as_ref().and_then(Option::as_ref).is_none() {
            thread.last_visited_at = Some(Some(Timestamp::from(iso_from_millis(now_millis() - 1))));
            emit(thread_event(ThreadChange::MetadataUpdated, &thread))?;
        }
        if thread.settled_override.as_ref().and_then(Option::as_ref).is_some() {
            thread.settled_override = Some(None);
            thread.settled_at = Some(None);
            thread.unsettled_at = Some(Some(at.clone()));
            thread.updated_at = at.clone();
            emit(thread_event(ThreadChange::Unsettled, &thread))?;
        }
        let seed = c.title_seed.as_deref().filter(|seed| !seed.trim().is_empty());
        if let Some(seed) = seed
            && !self.store.has_entities(tid, "messages")?
        {
            // The seed shows until the text generation model names the thread.
            thread.title = seed.to_owned();
            thread.title_regeneration = Some(Some(TitleRegeneration { request_id: command_id.clone(), started_at: at.clone() }));
            thread.updated_at = at.clone();
            emit(thread_event(ThreadChange::MetadataUpdated, &thread))?;
            self.send_effect(Effect::GenerateTitle { thread_id: tid.to_owned(), request_id: command_id.as_str().to_owned(), regenerate: false });
        }
        if let Some(selection) = c.model_selection.as_ref().filter(|selection| **selection != thread.model_selection) {
            let change =
                if selection.instance_id == thread.provider_instance_id { ThreadChange::ModelSelectionUpdated } else { ThreadChange::ProviderSwitched };
            thread.model_selection = selection.clone();
            thread.provider_instance_id = selection.instance_id.clone();
            thread.updated_at = at.clone();
            emit(thread_event(change, &thread))?;
        }
        let instance = thread.provider_instance_id.clone();
        let driver = &driver_for_instance(instance.as_str());
        let ordinal = runs.iter().map(|run| run.ordinal).max().unwrap_or(0) + 1;
        let rid = run_id(tid, ordinal);
        let attempt = attempt_id(&rid, 1);
        let node = root_node_id(&rid);
        let active = runs.iter().any(|run| is_active(&run.status) || run.status == RunStatus::Queued);

        // Provider thread: the conversation's for this instance, otherwise a new not-yet-loaded one.
        let reusable = if fresh { None } else { conversation_provider_thread(&self.store, tid, &outline, &thread, &instance) };
        let reused = reusable.is_some();
        let mut provider_thread = match reusable {
            Some(held) => {
                let mut held = held;
                held.last_run_ordinal = Some(ordinal);
                held.updated_at = at.clone();
                held
            }
            None => placeholder_provider_thread(tid, &instance, &rid, ordinal, &at),
        };
        let provider_thread_id = provider_thread.id.clone();
        // Context from elsewhere: a fork's or merge-back's pending transfer,
        // or the earlier runs when the provider changed (a new provider thread).
        let mut handoff: Option<(ContextHandoff, TurnItem)> = None;
        let mut transfer_update: Option<ContextTransfer> = None;
        let pending = outline.context_transfers.iter().find(|t| t.status == ContextTransferStatus::Pending && t.target_thread_id == *thread_id);
        if let Some(pending) = pending {
            let mut transfer = (**pending).clone();
            let merge_back = transfer.kind == ContextTransferType::MergeBack;
            let native_ref = transfer.source_point.provider_thread_ref.clone().filter(|native| native.native_id.is_some());
            let native_ref = native_ref.filter(|_| {
                transfer.kind == ContextTransferType::Fork
                    && !reused
                    && transfer.source_provider_instance_id.as_ref() == Some(&instance)
                    && can_native_fork(driver)
            });
            transfer.target_run_id = Some(rid.as_str().into());
            transfer.target_provider_instance_id = Some(instance.clone());
            transfer.updated_at = at.clone();
            transfer.consumed_at = Some(at.clone());
            transfer.status = ContextTransferStatus::Consumed;
            if let Some(provider_thread_ref) = native_ref {
                // The provider continues a copy of its own conversation.
                transfer.resolution = Some(ContextTransferResolution::NativeFork { provider_thread_ref });
                let source = self.store.thread_outline(transfer.source_thread_id.as_str())?;
                let source_run = source.runs.iter().find(|run| Some(&run.id) == transfer.source_point.run_id.as_ref());
                let from = source_run.and_then(|run| run.provider_thread_id.clone()).ok_or(Refusal::ForkSourceGone)?;
                provider_thread.forked_from = Some(ProviderThreadFork {
                    provider_thread_id: from,
                    provider_turn_id: source_run.and_then(|run| context::provider_turn_of(&source.attempts, &source.provider_turns, run)).map(|turn| turn.id.clone()),
                    checkpoint_id: transfer.source_point.checkpoint_id.clone(),
                });
            } else {
                // The conversation goes over as text.
                let source = self.store.load_thread(transfer.source_thread_id.as_str())?;
                let ordinal_of = |run: Option<&RunId>| source.as_ref().zip(run).and_then(|(source, run)| source.runs.get(run.as_str())).map(|run| run.ordinal);
                let to = ordinal_of(transfer.source_point.run_id.as_ref()).unwrap_or(u64::MAX);
                // What the fork inherited is already in the parent: a merge back hands over only its own runs.
                let from = ordinal_of(transfer.base_point.as_ref().and_then(|base| base.run_id.as_ref())).map_or(1, |base| base + 1);
                let (strategy, why) = if merge_back {
                    (HandoffStrategy::ForkDeltaSummary, "This thread continues after work done in a fork of it.")
                } else {
                    (HandoffStrategy::FullThreadSummary, "This thread is a fork of an earlier conversation, which you do not have.")
                };
                let target =
                    HandoffTarget { thread_id, run_id: &rid, provider_thread_id: &provider_thread_id, instance: &instance, transfer: Some(&transfer.id), at: &at };
                match source.as_ref().and_then(|source| handoff_for(source, from, to, &target, strategy, why)) {
                    Some(made) => {
                        let context_handoff_id = made.0.id.clone();
                        transfer.resolution = Some(if merge_back {
                            ContextTransferResolution::ForkDeltaContext { context_handoff_id }
                        } else {
                            ContextTransferResolution::PortableContext { context_handoff_id }
                        });
                        handoff = Some(made);
                    }
                    None => {
                        transfer.status = ContextTransferStatus::Failed;
                        transfer.error = Some("The source has no conversation to hand over.".into());
                    }
                }
            }
            transfer_update = Some(transfer);
        } else if !reused && !fresh {
            handoff = catch_up(&self.store, thread_id, &outline, &rid, ordinal, &provider_thread_id, &instance, &at)?;
        }
        if let Some((made, _)) = &handoff {
            provider_thread.handoff_ids.push(made.id.clone());
        }
        let queue_position = if restart_target.is_some() {
            // Restart: the new message goes first, once the stopped run settles.
            Some(0)
        } else if active {
            Some(runs.iter().filter(|run| run.status == RunStatus::Queued).filter_map(|run| run.queue_position.flatten()).max().unwrap_or(0) + 1)
        } else {
            None
        };
        if !active {
            emit(EventBody::ProviderThreadUpdated(Box::new(provider_thread)))?;
        }
        emit(EventBody::RunCreated(Box::new(Run {
            id: rid.as_str().into(),
            thread_id: thread_id.clone(),
            ordinal,
            provider_instance_id: instance.clone(),
            model_selection: thread.model_selection.clone(),
            provider_thread_id: (!active).then(|| provider_thread_id.clone()),
            user_message_id: c.message_id.clone(),
            root_node_id: Some(node.as_str().into()),
            active_attempt_id: Some(attempt.as_str().into()),
            status: if active { RunStatus::Queued } else { RunStatus::Starting },
            queue_position: Some(queue_position),
            queue_held: None,
            steer_when_ready_run_id: None,
            requested_at: at.clone(),
            started_at: None,
            completed_at: None,
            checkpoint_id: None,
            context_handoff_id: handoff.as_ref().map(|(made, _)| made.id.clone()),
            restart_continuation_of_run_id: c.restart_continuation_of_run_id.clone(),
            usage_limit_continuation_of_run_id: c.usage_limit_continuation_of_run_id.clone(),
            delegated_task_id: None,
            work_started_at: None,
            restart_cancelled_background_work: None,
            source_plan_ref: source_plan.as_ref().map(|(_, named)| (*named).clone()),
            delegated_completion: None,
            workspace_preparation: None,
            fresh_context: fresh.then_some(true),
        })))?;
        if let Some(transfer) = transfer_update {
            emit(EventBody::ContextTransferUpdated(Box::new(transfer)))?;
        }
        if let Some((made, item)) = handoff {
            emit(EventBody::ContextHandoffUpdated(Box::new(made)))?;
            emit(EventBody::TurnItemUpdated(item))?;
        }
        emit(EventBody::RunAttemptCreated(Box::new(RunAttempt {
            id: attempt.as_str().into(),
            native_thread_id: None,
            run_id: rid.as_str().into(),
            attempt_ordinal: 1,
            root_node_id: node.as_str().into(),
            provider_instance_id: instance.clone(),
            provider_thread_id: provider_thread_id.clone(),
            provider_turn_id: None,
            reason: RunAttemptReason::Initial,
            status: RunAttemptStatus::Pending,
            started_at: None,
            completed_at: None,
        })))?;
        let scope = scope_id(tid);
        emit(EventBody::NodeUpdated(Box::new(ExecutionNode {
            id: node.as_str().into(),
            thread_id: thread_id.clone(),
            run_id: Some(rid.as_str().into()),
            parent_node_id: None,
            root_node_id: node.as_str().into(),
            kind: NodeKind::RootTurn,
            status: NodeStatus::Pending,
            counts_for_run: true,
            provider_thread_id: Some(provider_thread_id.clone()),
            provider_turn_id: None,
            native_item_ref: None,
            runtime_request_id: None,
            checkpoint_scope_id: Some(scope.as_str().into()),
            started_at: None,
            completed_at: None,
        })))?;
        if outline.checkpoint_scopes.is_empty() {
            let project_root = self.store.get_project(thread.project_id.as_str()).ok().flatten().map(|project| project.workspace_root);
            emit(EventBody::CheckpointScopeCreated(Box::new(CheckpointScope {
                id: scope.as_str().into(),
                thread_id: thread_id.clone(),
                run_id: Some(rid.as_str().into()),
                node_id: node.as_str().into(),
                parent_scope_id: None,
                provider_thread_id: Some(provider_thread_id.clone()),
                kind: CheckpointScopeKind::RootRun,
                ordinal_within_parent: 0,
                advances_app_run_count: true,
                cwd: thread.worktree_path.clone().or(project_root).unwrap_or_else(|| ".".into()),
                created_at: at.clone(),
            })))?;
        }
        emit(EventBody::MessageUpdated(Box::new(user_message(thread_id, &rid, &node, &sent(InputIntent::TurnStart), &at))))?;
        if !active {
            emit(EventBody::TurnItemUpdated(user_turn_item(tid, &rid, &node, Some(&provider_thread_id), ordinal, sent(InputIntent::TurnStart), &at)))?;
            self.send_effect(Effect::StartTurn { thread_id: tid.to_owned(), run_id: rid });
        } else if let Some(target) = restart_target {
            self.send_effect(Effect::Interrupt { thread_id: tid.to_owned(), run_id: target.as_str().to_owned() });
        }
        self.answer(last)
    }

    /// Adds a message to a running run: the message and its `steer` turn
    /// item belong to that run, and the provider gets it live.
    pub(super) fn steer_into(&self, command_id: &CommandId, thread_id: &ThreadId, sent: Steered<'_>, outline: &Outline, run: &Run) -> Result<DispatchResult, Refusal> {
        let tid = thread_id.as_str();
        let rid = run.id.as_str();
        let origin = Self::run_origin(command_id, run);
        let at = now();
        let node = run.root_node_id.as_ref().map_or_else(|| root_node_id(rid), |node| node.as_str().to_owned());
        let message_id = sent.message_id.as_str().to_owned();
        self.store.append_event(tid, EventBody::MessageUpdated(Box::new(user_message(thread_id, rid, &node, &sent, &at))), &origin)?;
        // After what the run's timeline already holds.
        let base = run.ordinal * 1_000_000;
        let ordinal = self.store.last_item_ordinal(tid, base..base + 1_000_000)?.map_or(base + 1, |last| last + 1);
        let provider_turn = running_provider_turn(outline, run).map(|turn| turn.id.clone());
        let mut item = user_turn_item(tid, rid, &node, run.provider_thread_id.as_ref(), run.ordinal, sent, &at);
        if let TurnItem::UserMessage(item) = &mut item {
            item.ordinal = ordinal;
            item.provider_turn_id = provider_turn;
        }
        let sequence = self.store.append_event(tid, EventBody::TurnItemUpdated(item), &origin)?.sequence;
        self.send_effect(Effect::Steer { thread_id: tid.to_owned(), run_id: rid.to_owned(), message_id });
        self.answer(Some(sequence))
    }

    /// A steer the provider didn't take: the message becomes a queued run.
    pub async fn requeue_steer(&self, thread_id: &str, message_id: &str) {
        let _g = self.threads.lock(thread_id).await;
        let Ok(Some(message)) = self.store.record::<ConversationMessage>(thread_id, message_id) else { return };
        let mut queued =
            MessageDispatch::new(format!("steer-requeue:{message_id}"), thread_id, message_id, message.text, message.created_by, message.creation_source);
        queued.attachments = message.attachments;
        if let Err(refusal) = self.message_dispatch(&queued) {
            tracing::warn!(%thread_id, "could not queue a steer the provider refused: {refusal}");
        }
    }
}
