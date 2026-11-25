//! Commands about the work a run is doing: an answer to what the provider
//! asked, a stop of the run or of one of its subagents, and going back to a
//! checkpoint.

use super::{Answers, DispatchResult, Effect, Orchestrator, Refusal, is_active, now};
use crate::orchestration::events::{Origin, enc};
use crate::store::StoreError;
use domovoi_model::typed::command::{CheckpointRollback, MessageDispatch, RuntimeRequestRespond, SecretRequestRecord, SubagentStop};
use domovoi_model::typed::{
    Actor, CheckpointRollbackRequest, CommandId, CreationSource, EventBody, ExecutionNode, NodeStatus, ResponseCapability, RunStatus,
    RuntimeRequest, RuntimeRequestId, RuntimeRequestKind, RuntimeRequestStatus, SubagentStatus, ThreadChange, ThreadId, TurnItem,
    TurnItemStatus, UserInputAnswer, UserInputQuestion,
};

impl Orchestrator {
    /// `secret_request.record` (server-only): the card an agent's
    /// `request_secret` shows, new or updated. It holds the label, never the
    /// value. A card that ended stays as it ended; one asked under a run's
    /// earlier root (before a restart) can still close. A saved secret wakes
    /// a snoozed thread.
    pub(super) fn secret_request_record(&self, c: &SecretRequestRecord) -> Result<DispatchResult, Refusal> {
        use domovoi_model::typed::{SecretRequestItem, SecretRequestStatus};
        let thread = self.thread(&c.thread_id)?;
        let tid = c.thread_id.as_str();
        let outline = self.store.thread_outline(tid)?;
        let run = outline.runs.iter().find(|run| run.id == c.run_id).ok_or_else(|| Refusal::UnknownRun(c.run_id.clone()))?;
        let existing = match self.store.record::<TurnItem>(tid, c.turn_item_id.as_str())? {
            None => None,
            Some(TurnItem::SecretRequest(card)) => Some(card),
            Some(_) => return Err(Refusal::NotASecretRequest(c.turn_item_id.clone())),
        };
        if existing.is_none() && run.root_node_id.as_ref() != Some(&c.node_id) {
            return Err(Refusal::NotRunRoot { node: c.node_id.clone(), run: c.run_id.clone() });
        }
        if existing.as_ref().is_some_and(|card| card.run_id.as_ref() != Some(&c.run_id)) {
            return Err(Refusal::SecretRequestOfAnotherRun(c.turn_item_id.clone()));
        }
        let pending = c.secret_status == SecretRequestStatus::Pending;
        if existing.as_ref().is_some_and(|card| card.secret_status != SecretRequestStatus::Pending || pending) {
            return self.answer(None);
        }
        let at = now();
        let origin = Self::run_origin(&c.command_id, run);
        let (provider_thread_id, provider_turn_id, ordinal, started_at) = match &existing {
            Some(card) => (card.provider_thread_id.clone(), card.provider_turn_id.clone(), card.ordinal, card.started_at.clone()),
            None => (
                run.provider_thread_id.clone(),
                super::message::running_provider_turn(&outline, run).map(|turn| turn.id.clone()),
                super::next_item_ordinal(&self.store, tid, run.ordinal),
                Some(at.clone()),
            ),
        };
        let status = match c.secret_status {
            SecretRequestStatus::Pending => TurnItemStatus::Waiting,
            SecretRequestStatus::Saved => TurnItemStatus::Completed,
            _ => TurnItemStatus::Cancelled,
        };
        let card = TurnItem::SecretRequest(Box::new(SecretRequestItem {
            deferred_work: None,
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: c.turn_item_id.clone(),
            thread_id: c.thread_id.clone(),
            run_id: Some(c.run_id.clone()),
            node_id: Some(existing.as_ref().and_then(|card| card.node_id.clone()).unwrap_or_else(|| c.node_id.clone())),
            provider_thread_id,
            provider_turn_id,
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status,
            title: Some(c.label.clone()),
            started_at,
            completed_at: (!pending).then(|| at.clone()),
            updated_at: at.clone(),
            label: c.label.clone(),
            reason: c.reason.clone(),
            url: c.url.clone(),
            placeholder: c.placeholder.clone(),
            delivery: c.delivery.clone(),
            secret_status: c.secret_status.clone(),
        }));
        if c.secret_status == SecretRequestStatus::Saved && thread.snoozed_until.as_ref().and_then(Option::as_ref).is_some() {
            let mut awake = thread;
            awake.snoozed_until = Some(None);
            awake.snoozed_at = Some(None);
            awake.updated_at = at;
            self.store.append_event(tid, EventBody::Thread { change: ThreadChange::Unsnoozed, thread: Box::new(awake) }, &origin)?;
        }
        let sequence = self.store.append_event(tid, EventBody::TurnItemUpdated(card), &origin)?.sequence;
        self.answer(Some(sequence))
    }

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
    pub(super) fn respond(&self, thread_id: &ThreadId, request_id: &RuntimeRequestId, response: Option<&RuntimeRequestRespond>, command_id: &CommandId) -> Result<DispatchResult, Refusal> {
        let outline = self.store.thread_outline(thread_id.as_str())?;
        let request = outline.runtime_requests.iter().find(|request| request.id == *request_id).ok_or(Refusal::UnknownRuntimeRequest)?;
        if request.status != RuntimeRequestStatus::Pending {
            return Err(Refusal::RequestNotPending);
        }
        // Answers accept a question; dismissing it (or no answers) declines.
        let mut answers = response.and_then(|response| response.answers.clone());
        let fallback = if answers.is_some() { "accept" } else { "decline" };
        let decision = response.and_then(|response| response.decision.as_ref()).map_or(fallback, |decision| decision.as_str()).to_owned();
        let by_message = matches!(request.response_capability, ResponseCapability::Message) && request.kind == RuntimeRequestKind::UserInput;
        let open_code_form = request.kind == RuntimeRequestKind::UserInput
            && request.native_request_ref.as_ref().is_some_and(|native| native.driver.as_str() == "opencode");
        if by_message || open_code_form {
            let origin = Self::origin(command_id, None);
            let cancelled = matches!(decision.as_str(), "cancel" | "decline" | "stop");
            if by_message && cancelled {
                self.settle_message_question(thread_id, request, &origin, None)?;
                return self.answer(None);
            }
            if !cancelled {
                let Some(TurnItem::UserInputRequest(question)) = self.question_item(thread_id, request_id)? else {
                    return Err(Refusal::UnknownRuntimeRequest);
                };
                let normalized = normalize_answers(&question.questions, answers.as_ref()).ok_or(Refusal::InvalidQuestionAnswer)?;
                if by_message {
                    let text = question.questions.iter().filter_map(|question| {
                        let values = normalized.get(&question.id)?.as_array()?;
                        let labels = values.iter().filter_map(serde_json::Value::as_str).map(|value| {
                            question.options.iter().find(|option| option.value.as_deref() == Some(value)).map_or(value, |option| option.label.as_str())
                        }).collect::<Vec<_>>();
                        Some(format!("{}\n{}", question.question, labels.join(", ")))
                    }).collect::<Vec<_>>().join("\n\n");
                    if text.is_empty() {
                        return Err(Refusal::InvalidQuestionAnswer);
                    }
                    self.settle_message_question(thread_id, request, &origin, Some(normalized))?;
                    let mut reply = MessageDispatch::new(command_id.clone(), thread_id.clone(), format!("async-answer:{request_id}"), text, Actor::User, CreationSource::Server);
                    if let Some(attachments) = response.and_then(|response| response.attachments_by_question_id.as_ref()) {
                        reply.attachments.extend(attachments.values().flatten().cloned());
                    }
                    return self.message_dispatch_inner(&reply, true);
                }
                answers = Some(normalized);
            }
        }
        self.send_effect(Effect::RespondToRequest {
            thread_id: thread_id.as_str().to_owned(),
            request_id: request_id.as_str().to_owned(),
            decision,
            answers,
        });
        self.answer(None)
    }

    fn question_item(&self, thread_id: &ThreadId, request_id: &RuntimeRequestId) -> Result<Option<TurnItem>, StoreError> {
        let id = format!("turn-item:runtime-request:{}", enc(request_id.as_str()));
        self.store.record(thread_id.as_str(), &id)
    }

    fn settle_message_question(&self, thread_id: &ThreadId, request: &RuntimeRequest, origin: &Origin, answers: Option<Answers>) -> Result<(), StoreError> {
        let at = now();
        let tid = thread_id.as_str();
        let mut request = request.clone();
        let answered = answers.is_some();
        request.status = if answered { RuntimeRequestStatus::Resolved } else { RuntimeRequestStatus::Cancelled };
        request.decision = Some(if answered { domovoi_model::typed::ApprovalDecision::Accept } else { domovoi_model::typed::ApprovalDecision::Cancel });
        request.resolved_at = Some(at.clone());
        request.answers = answers.clone();
        if let Some(mut node) = self.store.record::<ExecutionNode>(tid, request.node_id.as_str())? {
            node.status = if answered { NodeStatus::Completed } else { NodeStatus::Cancelled };
            node.completed_at = Some(at.clone());
            self.store.append_event(tid, EventBody::NodeUpdated(Box::new(node)), origin)?;
        }
        if let Some(TurnItem::UserInputRequest(mut item)) = self.question_item(thread_id, &request.id)? {
            item.status = if answered { TurnItemStatus::Completed } else { TurnItemStatus::Cancelled };
            item.completed_at = Some(at.clone());
            item.updated_at = at;
            item.question_answer = answers.map(|answers| UserInputAnswer {
                request_id: request.id.as_str().into(),
                question_text_by_id: Some(item.questions.iter().map(|q| (q.id.clone(), q.question.clone())).collect()),
                answers,
                attachments_by_question_id: Default::default(),
            });
            self.store.append_event(tid, EventBody::TurnItemUpdated(TurnItem::UserInputRequest(item)), origin)?;
        }
        self.store.append_event(tid, EventBody::RuntimeRequestUpdated(Box::new(request)), origin)?;
        Ok(())
    }

    pub(super) fn close_message_questions(&self, thread_id: &ThreadId, requests: &[std::sync::Arc<RuntimeRequest>], origin: &Origin) -> Result<(), StoreError> {
        for request in requests {
            if request.kind == RuntimeRequestKind::UserInput && request.status == RuntimeRequestStatus::Pending
                && matches!(request.response_capability, ResponseCapability::Message)
            {
                self.settle_message_question(thread_id, request, origin, None)?;
            }
        }
        Ok(())
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
        self.close_message_questions(&c.thread_id, &outline.runtime_requests, &origin)?;
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

/// Normalize both web (strings or arrays) and native (arrays) clients to
/// arrays of option values or permitted custom text.
fn normalize_answers(questions: &[UserInputQuestion], answers: Option<&Answers>) -> Option<Answers> {
    use serde_json::Value;
    let answers = answers?;
    let mut normalized = Answers::new();
    for question in questions {
        let values = match answers.get(&question.id) {
            Some(Value::String(value)) => vec![value.as_str()],
            Some(Value::Array(values)) => values.iter().map(Value::as_str).collect::<Option<Vec<_>>>()?,
            Some(_) => return None,
            None => vec![],
        };
        let values = values.into_iter().filter(|value| !value.trim().is_empty()).collect::<Vec<_>>();
        if values.is_empty() {
            if question.required != Some(false) {
                return None;
            }
            continue;
        }
        if values.len() > 1 && question.multi_select != Some(true) {
            return None;
        }
        let mut canonical = Vec::new();
        for value in values {
            let value = match question.options.iter().find(|option| option.value.as_deref().unwrap_or(&option.label) == value) {
                Some(option) => option.value.as_deref().unwrap_or(&option.label),
                None if question.allow_custom_answer == Some(true) => value.trim(),
                None => return None,
            };
            if !canonical.iter().any(|prior| prior == value) {
                canonical.push(Value::String(value.into()));
            }
        }
        normalized.insert(question.id.clone(), Value::Array(canonical));
    }
    Some(normalized)
}
