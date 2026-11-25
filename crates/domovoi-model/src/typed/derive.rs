//! What is derived from a thread's records: the timeline as shown, and the
//! shell that sidebars and lists show. The rules are those of t3code's
//! projection (`createOrchestrationV2TurnItemVisibility`,
//! `threadShellFromProjection`) for threads without run-forks.
//!
//! A turn item is hidden when:
//!
//! - its run was rolled back;
//! - it is the user message of a queued turn whose run is still queued, or was cancelled;
//! - it is the result of an interrupt nobody asked for: its run has no
//!   interrupt request, and the attempt rooted at its node was superseded.
//!
//! The shell's status is that of the thread's latest run, where a queued run
//! that is held and a cancelled run that never started do not count.

use super::common::{Actor, ProviderFailureClass};
use super::ids::{ProviderInstanceId, RunId};
use super::message::{ConversationMessage, MessageRole};
use super::plan::{PlanArtifact, PlanStatus};
use super::provider::{ProviderThread, ResponseCapability, RuntimeRequestKind, RuntimeRequestStatus};
use super::run::{Run, RunAttemptStatus, RunStatus};
use super::shell::{ActivityRunStatus, PendingRuntimeRequestSummary, ShellThreadStatus, ThreadShell};
use super::state::Thread;
use super::turn_item::{InputIntent, ItemVisibility, ProjectedTurnItem, ProviderFailure, TurnItem, TurnItemStatus};
use std::sync::Arc;

impl From<&RunStatus> for ShellThreadStatus {
    fn from(status: &RunStatus) -> Self {
        match status {
            RunStatus::Preparing => Self::Preparing,
            RunStatus::Queued => Self::Queued,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Completed => Self::Completed,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::RolledBack => Self::RolledBack,
            RunStatus::Other(status) => Self::Other(status.clone()),
        }
    }
}

/// The status a run shows as while it makes its thread look busy.
fn activity_status(status: &RunStatus) -> Option<ActivityRunStatus> {
    match status {
        RunStatus::Preparing => Some(ActivityRunStatus::Preparing),
        RunStatus::Starting => Some(ActivityRunStatus::Starting),
        RunStatus::Running => Some(ActivityRunStatus::Running),
        RunStatus::Waiting => Some(ActivityRunStatus::Waiting),
        _ => None,
    }
}

impl Thread {
    /// The timeline as shown: the turn items that are not hidden, in order.
    pub fn visible_turn_item_refs(&self) -> Vec<&TurnItem> {
        self.visible().map(|item| &**item).collect()
    }

    /// The timeline as shown, as the server sends it: each item with its position.
    pub fn visible_turn_items(&self) -> Vec<ProjectedTurnItem> {
        self.visible()
            .zip(0..)
            .map(|(item, position)| ProjectedTurnItem {
                position,
                visibility: ItemVisibility::Local,
                source_thread_id: item.thread_id().clone(),
                source_item_id: item.id().clone(),
                item: Arc::clone(item),
            })
            .collect()
    }

    fn visible(&self) -> impl Iterator<Item = &Arc<TurnItem>> {
        self.turn_items.as_slice().iter().filter(|item| !self.is_hidden(item))
    }

    fn is_hidden(&self, item: &TurnItem) -> bool {
        let run_status = item.run_id().and_then(|run| self.runs.get(run)).map(|run| &run.status);
        match (item, run_status) {
            (_, Some(RunStatus::RolledBack)) => true,
            (TurnItem::UserMessage(message), Some(RunStatus::Queued | RunStatus::Cancelled)) => message.input_intent == InputIntent::QueuedTurn,
            (TurnItem::RunInterruptResult(_), _) => {
                let superseded = self.attempts.iter().any(|attempt| {
                    attempt.status == RunAttemptStatus::Superseded
                        && item.run_id() == Some(&attempt.run_id)
                        && item.node_id() == Some(&attempt.root_node_id)
                });
                superseded && !self.interrupt_requested(item.run_id())
            }
            _ => false,
        }
    }

    /// Whether the timeline holds a request to interrupt the run `run`.
    fn interrupt_requested(&self, run: Option<&RunId>) -> bool {
        self.turn_items.iter().any(|item| matches!(item, TurnItem::RunInterruptRequest(_)) && item.run_id() == run)
    }

    /// The newest run (highest ordinal, the earlier record if two share one) that `counts`.
    fn newest_run(&self, counts: impl Fn(&Run) -> bool) -> Option<&Run> {
        self.runs.iter().rev().filter(|run| counts(run)).max_by_key(|run| run.ordinal)
    }

    /// The thread as lists show it.
    pub fn shell(&self) -> ThreadShell {
        let thread = &self.record;
        let latest = self.newest_run(|run| {
            let held = run.status == RunStatus::Queued && run.queue_held == Some(true);
            let never_started = run.status == RunStatus::Cancelled && run.started_at.is_none();
            !held && !never_started
        });
        let active = self.newest_run(|run| matches!(run.status, RunStatus::Preparing | RunStatus::Starting | RunStatus::Running));
        let activity = self.newest_run(|run| activity_status(&run.status).is_some());

        // The error shown: the provider session's last one, else that of the
        // latest run if it failed (the newest failed `error` item at its root).
        let session_error = self
            .provider_sessions
            .iter()
            .filter(|session| session.provider_instance_id == thread.provider_instance_id)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
            .and_then(|session| session.last_error.as_deref());
        let failure: Option<&ProviderFailure> = latest.filter(|run| run.status == RunStatus::Failed).and_then(|run| {
            self.turn_items.iter().rev().find_map(|item| match item {
                TurnItem::Error(error)
                    if error.status == TurnItemStatus::Failed
                        && error.run_id.as_ref() == Some(&run.id)
                        && error.node_id == run.root_node_id =>
                {
                    Some(&error.failure)
                }
                _ => None,
            })
        });
        // The failure's details are shown only when the error shown is the failure's.
        let detailed = failure.filter(|failure| session_error.is_none_or(|error| error == failure.message));
        let class = detailed.map(|failure| failure.class.clone());

        // The request the thread waits on: one that blocks the agent before a
        // question to the user, then the newest.
        let pending = self
            .runtime_requests
            .iter()
            .filter(|request| request.status == RuntimeRequestStatus::Pending)
            .min_by(|a, b| {
                let is_question = |kind: &RuntimeRequestKind| *kind == RuntimeRequestKind::UserInput;
                is_question(&a.kind).cmp(&is_question(&b.kind)).then_with(|| b.created_at.cmp(&a.created_at))
            })
            .map(|request| PendingRuntimeRequestSummary {
                id: request.id.clone(),
                kind: request.kind.clone(),
                created_at: request.created_at.clone(),
                blocking: Some(!matches!(request.response_capability, ResponseCapability::Message)),
                user_only: None,
            });

        let from_user = |message: &&ConversationMessage| message.role == MessageRole::User && message.idle_compaction.is_none();
        let latest_user_message_at = self.messages.iter().filter(from_user).map(|message| &message.updated_at).max().cloned();
        let latest_user_authored_message_at = self
            .messages
            .iter()
            .filter(from_user)
            .filter(|message| message.created_by == Actor::User)
            .map(|message| &message.updated_at)
            .max()
            .cloned();

        // Every provider the thread itself has talked to, in the order it first did.
        let mut own_provider_threads: Vec<&ProviderThread> = self
            .provider_threads
            .iter()
            .filter(|provider_thread| provider_thread.app_thread_id.as_ref() == Some(&thread.id) && provider_thread.owner_node_id.is_none())
            .collect();
        own_provider_threads.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        let mut provider_instance_history: Vec<ProviderInstanceId> = Vec::new();
        for provider_thread in own_provider_threads {
            if !provider_instance_history.contains(&provider_thread.provider_instance_id) {
                provider_instance_history.push(provider_thread.provider_instance_id.clone());
            }
        }
        let goal = thread
            .active_provider_thread_id
            .as_ref()
            .and_then(|id| self.provider_threads.get(id))
            .and_then(|provider_thread| provider_thread.goal.clone().flatten());

        let visible_item_count = self.visible().count() as u64;
        ThreadShell {
            created_by: thread.created_by.clone(),
            creation_source: thread.creation_source.clone(),
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            provider_instance_id: thread.provider_instance_id.clone(),
            model_selection: thread.model_selection.clone(),
            runtime_mode: thread.runtime_mode.clone(),
            interaction_mode: thread.interaction_mode.clone(),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            linked_pull_request: None,
            pull_requests: Some(thread.pull_requests.clone().unwrap_or_default()),
            issues: thread.issues.clone(),
            branch_pull_request: None,
            lineage: thread.lineage.clone(),
            forked_from: thread.forked_from.clone(),
            active_provider_thread_id: thread.active_provider_thread_id.clone(),
            history_origin: None,
            link_origin: None,
            delegated_from: None,
            latest_run_id: latest.map(|run| run.id.clone()),
            latest_run_requested_at: Some(latest.map(|run| run.requested_at.clone())),
            latest_run_started_at: Some(latest.and_then(|run| run.started_at.clone())),
            latest_run_completed_at: Some(latest.and_then(|run| run.completed_at.clone())),
            latest_run_stop_requested: Some(latest.is_some_and(|run| self.interrupt_requested(Some(&run.id)))),
            latest_run_background_wake: Some(false),
            active_run_id: active.map(|run| run.id.clone()),
            activity_run_started_at: Some(activity.map(|run| run.work_started_at.as_ref().or(run.started_at.as_ref()).unwrap_or(&run.requested_at).clone())),
            activity_run_status: Some(activity.and_then(|run| activity_status(&run.status))),
            blocked_on_thread_wait: Some(false),
            status: latest.map_or(ShellThreadStatus::Idle, |run| ShellThreadStatus::from(&run.status)),
            last_error: Some(session_error.or(failure.map(|failure| failure.message.as_str())).map(str::to_owned)),
            last_error_code: Some(detailed.and_then(|failure| failure.code.clone())),
            usage_limit_reset_at: Some(
                detailed.filter(|failure| failure.class == ProviderFailureClass::UsageLimit).and_then(|failure| failure.reset_at.clone().flatten()),
            ),
            last_error_class: Some(class),
            pending_runtime_request: pending,
            pending_guest_message_count: Some(0),
            latest_visible_message: None,
            latest_user_message_at,
            latest_user_authored_message_at: Some(latest_user_authored_message_at),
            has_actionable_proposed_plan: self
                .plans
                .iter()
                .any(|plan| matches!(plan, PlanArtifact::ProposedPlan(plan) if plan.status == PlanStatus::Active)),
            pending_background_tasks: Some(Vec::new()),
            provider_instance_history: Some(provider_instance_history),
            goal: Some(goal),
            item_count: visible_item_count,
            visible_item_count,
            created_at: thread.created_at.clone(),
            updated_at: thread.updated_at.clone(),
            archived_at: thread.archived_at.clone(),
            settled_override: thread.settled_override.clone().flatten(),
            settled_at: thread.settled_at.clone().flatten(),
            unsettled_at: Some(thread.unsettled_at.clone().flatten()),
            snoozed_until: Some(thread.snoozed_until.clone().flatten()),
            snoozed_at: Some(thread.snoozed_at.clone().flatten()),
            limit_recovery: Some(thread.limit_recovery.clone().flatten()),
            pinned_at: Some(thread.pinned_at.clone().flatten()),
            auto_settle_disabled_at: Some(thread.auto_settle_disabled_at.clone().flatten()),
            pin_order_key: Some(thread.pin_order_key.clone().flatten()),
            active_order_key: None,
            tags: None,
            last_visited_at: Some(thread.last_visited_at.clone().flatten()),
            opens: Some(thread.opens.clone().flatten()),
            title_regeneration: Some(thread.title_regeneration.clone().flatten()),
            last_title_regeneration: Some(thread.last_title_regeneration.clone().flatten()),
            deleted_at: thread.deleted_at.clone(),
        }
    }
}
