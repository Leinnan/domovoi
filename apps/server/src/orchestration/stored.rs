//! Reading a thread's stored JSON: every record and event the store holds is
//! read through here, into its type.
//!
//! A row this server wrote is decoded straight from its text. The model
//! decodes an event, and a turn item, by first reading the JSON into a map,
//! because the `type` that says what the rest is may come anywhere. In a row
//! this server wrote it comes where it is looked for here, so the record is
//! read once, into its type. Any other row is decoded as the model does it:
//! the result is the same either way.

use serde::Deserialize;
use serde::de::Error;
use std::borrow::Cow;
use std::sync::Arc;
use domovoi_model::typed::{
    DomainEvent, EventBody, EventId, NodeId, ProviderDriverKind, ProviderInstanceId, RawEventId, RecordKind, RunId, StoredRecord, ThreadChange,
    ThreadId, Timestamp, TurnItem,
};

/// What comes before an event's record in its JSON, when the record is the last thing in it.
pub(super) const PAYLOAD_KEY: &str = ",\"payload\":";

/// A stored record of the given kind.
pub(super) fn record(kind: RecordKind, text: &str) -> serde_json::Result<StoredRecord> {
    match kind {
        RecordKind::TurnItem => turn_item(text).map(|item| StoredRecord::TurnItem(Arc::new(item))),
        kind => StoredRecord::from_json(kind, text),
    }
}

/// A stored event.
pub(super) fn event(text: &str) -> serde_json::Result<DomainEvent> {
    event_written_here(text).or_else(|_| serde_json::from_str(text))
}

/// Declares the turn item types read straight from their text, each with its record.
macro_rules! turn_items_by_type {
    ($($tag:literal => $variant:ident($record:ident),)+) => {
        /// A turn item whose JSON starts with a `type` this build knows, read
        /// straight into that type's record.
        fn turn_item_written_here(text: &str) -> serde_json::Result<TurnItem> {
            let tag = text.strip_prefix("{\"type\":\"").and_then(|rest| rest.split_once('"')).map(|(tag, _)| tag);
            match tag {
                $(Some($tag) => serde_json::from_str::<domovoi_model::typed::$record>(text).map(|record| TurnItem::$variant(Box::new(record))),)+
                _ => Err(serde_json::Error::custom("not a turn item this server wrote")),
            }
        }
    };
}

turn_items_by_type! {
    "notification" => Notification(NotificationItem),
    "user_message" => UserMessage(UserMessageItem),
    "assistant_message" => AssistantMessage(AssistantMessageItem),
    "reasoning" => Reasoning(ReasoningItem),
    "proposed_plan" => ProposedPlan(ProposedPlanItem),
    "todo_list" => TodoList(TodoListItem),
    "user_input_request" => UserInputRequest(UserInputRequestItem),
    "file_change" => FileChange(FileChangeItem),
    "command_execution" => CommandExecution(CommandExecutionItem),
    "file_search" => FileSearch(FileSearchItem),
    "web_search" => WebSearch(WebSearchItem),
    "approval_request" => ApprovalRequest(ApprovalRequestItem),
    "checkpoint" => Checkpoint(CheckpointItem),
    "run_interrupt_request" => RunInterruptRequest(RunInterruptRequestItem),
    "run_interrupt_result" => RunInterruptResult(RunInterruptResultItem),
    "system_notice" => SystemNotice(SystemNoticeItem),
    "error" => Error(ErrorItem),
    "compaction" => Compaction(CompactionItem),
    "handoff" => Handoff(HandoffItem),
    "fork" => Fork(ForkItem),
    "thread_created" => ThreadCreated(ThreadCreatedItem),
    "secret_request" => SecretRequest(SecretRequestItem),
    "link_request" => LinkRequest(LinkRequestItem),
    "subagent" => Subagent(SubagentItem),
    "dynamic_tool" => DynamicTool(DynamicToolItem),
}

/// A turn item from its JSON.
fn turn_item(text: &str) -> serde_json::Result<TurnItem> {
    turn_item_written_here(text).or_else(|_| serde_json::from_str(text))
}

/// An event's envelope: everything in it but its record.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope<'a> {
    id: EventId,
    thread_id: ThreadId,
    #[serde(default)]
    run_id: Option<RunId>,
    #[serde(default)]
    node_id: Option<NodeId>,
    #[serde(default)]
    driver: Option<ProviderDriverKind>,
    #[serde(default)]
    provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default)]
    raw_event_id: Option<RawEventId>,
    occurred_at: Timestamp,
    #[serde(rename = "type", borrow)]
    event_type: Cow<'a, str>,
}

/// An event whose JSON is an envelope of plain fields and then its record,
/// last, as this server writes events: the envelope and the record are each
/// read straight into their types.
fn event_written_here(text: &str) -> serde_json::Result<DomainEvent> {
    // In the envelope's strings a quote is written `\"`: the first `,"payload":` is the key.
    let at = memchr::memmem::find(text.as_bytes(), PAYLOAD_KEY.as_bytes()).ok_or_else(|| serde_json::Error::custom("no record"))?;
    let record = text[at + PAYLOAD_KEY.len()..].strip_suffix('}').ok_or_else(|| serde_json::Error::custom("not an object"))?;
    let mut envelope_text = String::with_capacity(at + 1);
    envelope_text.push_str(&text[..at]);
    envelope_text.push('}');
    let envelope: Envelope<'_> = serde_json::from_str(&envelope_text)?;
    Ok(DomainEvent {
        body: body(&envelope.event_type, record)?,
        id: envelope.id,
        thread_id: envelope.thread_id,
        run_id: envelope.run_id,
        node_id: envelope.node_id,
        driver: envelope.driver,
        provider_instance_id: envelope.provider_instance_id,
        raw_event_id: envelope.raw_event_id,
        occurred_at: envelope.occurred_at,
    })
}

/// What an event of type `event_type` says, from the JSON of the record it carries.
fn body(event_type: &str, record: &str) -> serde_json::Result<EventBody> {
    if let Some(change) = ThreadChange::of_event_type(event_type) {
        return Ok(EventBody::Thread { change, thread: serde_json::from_str(record)? });
    }
    Ok(match event_type {
        "thread.pull-request-link-updated" => EventBody::ThreadPullRequestLinkUpdated(serde_json::from_str(record)?),
        "run.created" => EventBody::RunCreated(serde_json::from_str(record)?),
        "run.updated" => EventBody::RunUpdated(serde_json::from_str(record)?),
        "run.background-work-cancelled" => EventBody::RunBackgroundWorkCancelled(serde_json::from_str(record)?),
        "run-attempt.created" => EventBody::RunAttemptCreated(serde_json::from_str(record)?),
        "run-attempt.updated" => EventBody::RunAttemptUpdated(serde_json::from_str(record)?),
        "node.updated" => EventBody::NodeUpdated(serde_json::from_str(record)?),
        "subagent.updated" => EventBody::SubagentUpdated(serde_json::from_str(record)?),
        "provider-session.attached" => EventBody::ProviderSessionAttached(serde_json::from_str(record)?),
        "provider-session.updated" => EventBody::ProviderSessionUpdated(serde_json::from_str(record)?),
        "provider-session.detached" => EventBody::ProviderSessionDetached(serde_json::from_str(record)?),
        "provider-thread.updated" => EventBody::ProviderThreadUpdated(serde_json::from_str(record)?),
        "provider-turn.updated" => EventBody::ProviderTurnUpdated(serde_json::from_str(record)?),
        "runtime-request.updated" => EventBody::RuntimeRequestUpdated(serde_json::from_str(record)?),
        "message.updated" => EventBody::MessageUpdated(serde_json::from_str(record)?),
        "turn-item.updated" => EventBody::TurnItemUpdated(turn_item(record)?),
        "plan.updated" => EventBody::PlanUpdated(serde_json::from_str(record)?),
        "checkpoint-scope.created" => EventBody::CheckpointScopeCreated(serde_json::from_str(record)?),
        "checkpoint.captured" => EventBody::CheckpointCaptured(serde_json::from_str(record)?),
        "checkpoint.rollback-requested" => EventBody::CheckpointRollbackRequested(serde_json::from_str(record)?),
        "context-handoff.updated" => EventBody::ContextHandoffUpdated(serde_json::from_str(record)?),
        "context-transfer.created" => EventBody::ContextTransferCreated(serde_json::from_str(record)?),
        "context-transfer.updated" => EventBody::ContextTransferUpdated(serde_json::from_str(record)?),
        _ => return Err(serde_json::Error::custom("not an event this build knows")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_records::{AT, assistant_item, message, run, thread, with};
    use serde_json::{Value, json};

    fn envelope(event_type: &str, payload: &Value) -> String {
        json!({ "id": "e1", "threadId": "t,\"payload\":{", "driver": "codex", "providerInstanceId": "codex", "occurredAt": AT, "type": event_type, "payload": payload }).to_string()
    }

    /// Read straight from its text or as the model reads it, an event is the same event.
    #[test]
    fn an_event_read_straight_from_its_text_is_the_event_the_model_reads() {
        let plan = json!({ "id": "p1", "threadId": "t1", "runId": null, "nodeId": "n1", "kind": "todo_list", "status": "active", "steps": [] });
        let events = [
            ("thread.created", thread("t1", "p")),
            ("thread.archived", with(thread("t1", "p"), json!({ "archivedAt": AT, "opens": { "openedAt": 1.5, "score": 2, "picks": [] } }))),
            ("run.created", run("t1", "r1", "queued")),
            ("message.updated", message("t1", "m1", "assistant", "say \"hi\" — ,\"payload\":{ \u{1F600}", true)),
            ("turn-item.updated", assistant_item("t1", "i1", "m1", 3, "text", false)),
            ("turn-item.updated", json!({ "id": "i2", "threadId": "t1", "ordinal": 4, "status": "completed", "updatedAt": AT, "type": "dynamic_tool",
                "toolName": "t", "input": { "deep": [1, 2.5, null, { "type": "x" }] }, "output": null })),
            ("turn-item.updated", json!({ "type": "a type of a newer server", "id": "i3", "threadId": "t1", "ordinal": 5, "status": "completed", "updatedAt": AT, "more": 1 })),
            ("plan.updated", plan),
            ("checkpoint.rollback-requested", json!({ "scopeId": "s", "checkpointId": "c", "requestedAt": AT })),
            ("provider-session.detached", json!({ "providerSessionId": "s", "detachedAt": AT })),
        ];
        for (event_type, payload) in events {
            // As the server writes it, and as it reads back what it wrote.
            let given = envelope(event_type, &payload);
            let by_model: DomainEvent = serde_json::from_str(&given).unwrap();
            let written = serde_json::to_string(&by_model).unwrap();
            for text in [&given, &written] {
                assert_eq!(event_written_here(text).unwrap(), by_model, "{event_type}: {text}");
                assert_eq!(event(text).unwrap(), by_model);
            }
        }
    }

    /// An event that is not laid out as this server writes one is still read, as the model reads it.
    #[test]
    fn an_event_laid_out_otherwise_is_read_as_the_model_reads_it() {
        let record = run("t1", "r1", "queued");
        for text in [
            // The record first, the envelope after it.
            json!({ "payload": record, "type": "run.created", "id": "e1", "threadId": "t1", "occurredAt": AT }).to_string(),
            // A field of the envelope this build does not know.
            json!({ "id": "e1", "threadId": "t1", "occurredAt": AT, "later": { "payload": 1 }, "type": "run.created", "payload": record }).to_string(),
            // A type this build does not know.
            json!({ "id": "e1", "threadId": "t1", "occurredAt": AT, "type": "run.paused", "payload": record }).to_string(),
        ] {
            assert!(event_written_here(&text).is_err(), "{text}");
            assert_eq!(event(&text).unwrap(), serde_json::from_str::<DomainEvent>(&text).unwrap());
        }
        assert!(event("{\"type\":\"run.created\"}").is_err(), "an event the model does not read is not read here either");
    }

    #[test]
    fn a_turn_item_read_straight_from_its_text_is_the_item_the_model_reads() {
        let shared = json!({ "id": "i1", "threadId": "t1", "runId": "r1", "ordinal": 7, "status": "completed", "updatedAt": AT });
        let items = [
            json!({ "type": "user_message", "createdBy": "user", "creationSource": "web", "messageId": "m1", "inputIntent": "turn_start", "text": "hi", "attachments": [] }),
            json!({ "type": "command_execution", "input": "ls", "output": "a\nb", "exitCode": 0, "completionWake": "always" }),
            json!({ "type": "file_change", "fileName": "a.txt", "diffStr": "@@", "changes": [{ "operation": "update", "path": "a.txt" }] }),
            json!({ "type": "error", "failure": { "class": "provider_error", "message": "boom", "code": null, "retryable": null } }),
            json!({ "type": "fork", "source": { "type": "node", "nodeId": "n1" }, "targetThreadId": "t2" }),
            json!({ "type": "something newer", "extra": [1, 2] }),
        ];
        for own in items {
            let given = with(shared.clone(), own).to_string();
            let by_model: TurnItem = serde_json::from_str(&given).unwrap();
            let written = serde_json::to_string(&by_model).unwrap();
            assert!(written.starts_with("{\"type\":") || by_model.item_type() == "something newer", "a typed item is written with its type first: {written}");
            assert_eq!(turn_item(&given).unwrap(), by_model);
            assert_eq!(turn_item(&written).unwrap(), by_model, "{written}");
            if by_model.item_type() != "something newer" {
                assert_eq!(turn_item_written_here(&written).unwrap(), by_model);
            }
        }
        assert!(turn_item("{\"type\":\"reasoning\"}").is_err());
    }
}
