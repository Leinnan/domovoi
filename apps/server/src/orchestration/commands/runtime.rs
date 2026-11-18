//! Commands about the work a run is doing: an answer to what the provider
//! asked, a stop of the run or of one of its subagents, and going back to a
//! checkpoint.

use super::{DispatchResult, Effect, Orchestrator, Refusal, is_active, now};
use domovoi_model::typed::command::{CheckpointRollback, RuntimeRequestRespond, SubagentStop};
use domovoi_model::typed::{
    CheckpointRollbackRequest, CommandId, EventBody, RunStatus, RuntimeRequestId, RuntimeRequestStatus, SubagentStatus, ThreadChange, ThreadId,
};

impl Orchestrator {
    /// `subagent.stop`: the provider stops that subagent; the turn goes on.
    pub(super) fn stop_subagent(&self, c: &SubagentStop) -> Result<DispatchResult, Refusal> {
        self.thread(&c.thread_id)?;
        let outline = self.store.thread_outline(c.thread_id.as_str())?;
        let subagent = outline.subagents.iter().find(|subagent| subagent.id == c.subagent_id).ok_or(Refusal::UnknownSubagent)?;
        if !matches!(subagent.status, SubagentStatus::Running | SubagentStatus::Pending | SubagentStatus::Waiting) {
            return Err(Refusal::SubagentNotRunning);
        }
        let native_id = subagent.native_task_ref.as_ref().and_then(|native| native.native_id.clone()).ok_or(Refusal::SubagentWithoutNativeId)?;
        self.send_effect(Effect::StopSubagent { thread_id: c.thread_id.as_str().to_owned(), native_id });
        self.answer(None)
    }

    /// `runtime-request.respond`, or `thread.user-input.dismiss` (no
    /// `response`): the provider gets the user's answer to what it asked.
    pub(super) fn respond(&self, thread_id: &ThreadId, request_id: &RuntimeRequestId, response: Option<&RuntimeRequestRespond>) -> Result<DispatchResult, Refusal> {
        let outline = self.store.thread_outline(thread_id.as_str())?;
        let request = outline.runtime_requests.iter().find(|request| request.id == *request_id).ok_or(Refusal::UnknownRuntimeRequest)?;
        if request.status != RuntimeRequestStatus::Pending {
            return Err(Refusal::RequestNotPending);
        }
        // Answers accept a question; dismissing it (or no answers) declines.
        let answers = response.and_then(|response| response.answers.clone());
        let fallback = if answers.is_some() { "accept" } else { "decline" };
        let decision = response.and_then(|response| response.decision.as_ref()).map_or(fallback, |decision| decision.as_str()).to_owned();
        self.send_effect(Effect::RespondToRequest {
            thread_id: thread_id.as_str().to_owned(),
            request_id: request_id.as_str().to_owned(),
            decision,
            answers,
        });
        self.answer(None)
    }

    /// `thread.stop` and `run.interrupt`: stops the active run. With
    /// `hold_queue`, what is queued then waits for the user's next message.
    pub(super) fn interrupt(&self, thread_id: &ThreadId, command_id: &CommandId, hold_queue: bool) -> Result<DispatchResult, Refusal> {
        let outline = self.store.thread_outline(thread_id.as_str())?;
        let active = outline.runs.iter().find(|run| is_active(&run.status)).ok_or(Refusal::NoActiveRun)?;
        if hold_queue {
            for queued in outline.runs.iter().filter(|run| run.status == RunStatus::Queued && run.queue_held != Some(true)) {
                let mut run = (**queued).clone();
                run.queue_held = Some(true);
                self.store.append_event(thread_id.as_str(), EventBody::RunUpdated(Box::new(run)), &Self::run_origin(command_id, queued))?;
            }
        }
        self.send_effect(Effect::Interrupt { thread_id: thread_id.as_str().to_owned(), run_id: active.id.as_str().to_owned() });
        self.answer(None)
    }

    /// `checkpoint.rollback`: notes the request on the thread; the effect
    /// rewinds the provider's conversation and, if asked, the files.
    pub(super) fn rollback(&self, c: &CheckpointRollback) -> Result<DispatchResult, Refusal> {
        let thread_id = c.thread_id.as_str();
        let outline = self.store.thread_outline(thread_id)?;
        if !outline.checkpoints.iter().any(|checkpoint| checkpoint.id == c.checkpoint_id) {
            return Err(Refusal::UnknownCheckpoint);
        }
        if outline.runs.iter().any(|run| is_active(&run.status)) {
            return Err(Refusal::RunStillActive);
        }
        let mut thread = self.thread(&c.thread_id)?;
        let restore_files = c.restore_files == Some(true);
        if restore_files && thread.worktree_path.is_none() {
            return Err(Refusal::RestoreNeedsWorktree);
        }
        thread.rollback_request_id = Some(c.command_id.clone());
        thread.rollback_failure = Some(None);
        // Not on a provider's behalf: the rollback is the user's.
        let origin = Self::origin(&c.command_id, None);
        self.store.append_event(thread_id, EventBody::Thread { change: ThreadChange::MetadataUpdated, thread: Box::new(thread) }, &origin)?;
        let requested =
            CheckpointRollbackRequest { scope_id: c.scope_id.clone(), checkpoint_id: c.checkpoint_id.clone(), requested_at: now() };
        let sequence = self.store.append_event(thread_id, EventBody::CheckpointRollbackRequested(requested), &origin)?.sequence;
        self.send_effect(Effect::Rollback {
            thread_id: thread_id.to_owned(),
            checkpoint_id: c.checkpoint_id.as_str().to_owned(),
            restore_files,
            command_id: c.command_id.as_str().to_owned(),
        });
        self.answer(Some(sequence))
    }
}
