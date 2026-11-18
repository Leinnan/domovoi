//! Records for tests, each one the schema allows: whole, and written as the
//! types write them, so a record read back from the store equals the one
//! given. A test changes the fields it is about with `with`.
//!
//! A test that is about the JSON of records gives and reads them as JSON
//! here, at its own edge: `append` types an event given as JSON before it
//! reaches the store, and `stored` reads back the text a record is kept as.

use crate::orchestration::events::Origin;
use crate::store::{Store, StoreError, StoredEvent};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use domovoi_model::typed::{
    AppThread, ConversationMessage, CreationSource, DomainEvent, EventBody, ExecutionNode, InitialMessage, InteractionMode, ModelSelection, RecordKind,
    Run, RunAttempt, RuntimeMode, RuntimeRequest, ThreadLaunchInput, TurnItem, WorkspaceStrategy,
};

/// When every record here was made.
pub const AT: &str = "2026-11-16T00:00:00.000Z";

/// `record` as its type `T` writes it. Panics if it is not a `T`: the test's data is wrong.
pub fn written_as<T: Serialize + DeserializeOwned>(record: Value) -> Value {
    json!(serde_json::from_value::<T>(record).expect("the test's record fits the schema"))
}

/// What an event of type `event_type` carrying `payload` says. Panics if the
/// schema has no such event, or `payload` is not what it carries: the test's data is wrong.
pub fn event_body(thread_id: &str, event_type: &str, payload: Value) -> EventBody {
    let event = json!({ "id": "test", "threadId": thread_id, "occurredAt": AT, "type": event_type, "payload": payload });
    let event = DomainEvent::from_value(event).unwrap_or_else(|e| panic!("the test's `{event_type}` event does not fit the schema: {e}"));
    assert!(!matches!(event.body, EventBody::Unknown(_)), "the schema has no `{event_type}` event");
    event.body
}

/// Appends to `thread_id` the event of type `event_type` whose record is given as JSON.
pub fn append(store: &Store, thread_id: &str, event_type: &str, payload: Value, origin: &Origin) -> Result<Arc<StoredEvent>, StoreError> {
    store.append_event(thread_id, event_body(thread_id, event_type, payload), origin)
}

/// A stored record of the thread as the JSON it is kept as. Panics if there is none.
pub fn stored(store: &Store, thread_id: &str, kind: RecordKind, id: &str) -> Value {
    let text: String = store
        .db()
        .read(|c| c.query_row("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 AND entity_id = ?3", [thread_id, kind.collection(), id], |r| r.get(0)))
        .unwrap_or_else(|e| panic!("no {} {id} in {thread_id}: {e}", kind.collection()));
    serde_json::from_str(&text).expect("a stored record is JSON")
}

/// The thread's stored records of `kind` as JSON, in the order they first appeared.
pub fn stored_of_kind(store: &Store, thread_id: &str, kind: RecordKind) -> Vec<Value> {
    store
        .db()
        .read(|c| {
            let mut st = c.prepare("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 ORDER BY first_sequence")?;
            let rows = st.query_map([thread_id, kind.collection()], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .expect("records")
        .iter()
        .map(|text| serde_json::from_str(text).expect("a stored record is JSON"))
        .collect()
}

/// The thread's own record as the JSON it is kept as, if it has one.
pub fn stored_thread(store: &Store, thread_id: &str) -> Option<Value> {
    use rusqlite::OptionalExtension;
    let text: Option<String> = store.db().read(|c| c.query_row("SELECT record FROM threads WHERE id = ?1", [thread_id], |r| r.get(0)).optional()).expect("thread");
    text.map(|text| serde_json::from_str(&text).expect("a stored record is JSON"))
}

/// `record` with each of `fields` in place of its own.
pub fn with(mut record: Value, fields: Value) -> Value {
    for (key, value) in fields.as_object().into_iter().flatten() {
        record[key.as_str()] = value.clone();
    }
    record
}

/// The launch of thread `thread_id` in project `project_id`, in the project's
/// folder, on provider instance `instance`; its first message `m1` says `text`.
pub fn launch(command_id: &str, thread_id: &str, project_id: &str, instance: &str, text: &str) -> ThreadLaunchInput {
    ThreadLaunchInput {
        command_id: command_id.into(),
        creation_source: Some(CreationSource::Web),
        thread_id: Some(thread_id.into()),
        reuse_existing_thread: None,
        project_id: project_id.into(),
        title: "Thread".into(),
        generate_title: Some(false),
        model_selection: ModelSelection { instance_id: instance.into(), model: "gpt-6-luna".into(), options: None },
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Default,
        workspace_strategy: WorkspaceStrategy::Root { branch: None },
        initial_message: Some(InitialMessage { message_id: Some("m1".into()), text: text.into(), context: None, attachments: vec![] }),
        created_by: None,
        delegated_from: None,
    }
}

/// A thread of project `project_id`, as a user creates one.
pub fn thread(id: &str, project_id: &str) -> Value {
    written_as::<AppThread>(json!({
        "createdBy": "user", "creationSource": "web", "id": id, "projectId": project_id, "title": "Thread", "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-6-luna" }, "runtimeMode": "approval-required", "interactionMode": "default",
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": id }, "createdAt": AT, "updatedAt": AT,
    }))
}

/// The thread's first run, in `status`.
pub fn run(thread_id: &str, id: &str, status: &str) -> Value {
    written_as::<Run>(json!({
        "id": id, "threadId": thread_id, "ordinal": 1, "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "gpt-6-luna" },
        "userMessageId": format!("message-of-{id}"), "status": status, "requestedAt": AT,
    }))
}

/// A run's root node, in `status`.
pub fn node(thread_id: &str, id: &str, status: &str) -> Value {
    written_as::<ExecutionNode>(json!({
        "id": id, "threadId": thread_id, "rootNodeId": id, "kind": "root_turn", "status": status, "countsForRun": true, "startedAt": AT,
    }))
}

/// The first attempt at `run_id`, in `status`.
pub fn attempt(run_id: &str, id: &str, status: &str) -> Value {
    written_as::<RunAttempt>(json!({
        "id": id, "runId": run_id, "attemptOrdinal": 1, "rootNodeId": format!("node-of-{run_id}"), "providerInstanceId": "codex",
        "providerThreadId": format!("provider-thread-of-{run_id}"), "reason": "initial", "status": status, "startedAt": AT,
    }))
}

/// A message `role` wrote.
pub fn message(thread_id: &str, id: &str, role: &str, text: &str, streaming: bool) -> Value {
    written_as::<ConversationMessage>(json!({
        "createdBy": if role == "user" { "user" } else { "agent" }, "creationSource": if role == "user" { "web" } else { "provider" },
        "id": id, "threadId": thread_id, "role": role, "text": text, "attachments": [], "streaming": streaming, "createdAt": AT, "updatedAt": AT,
    }))
}

/// The turn item that shows the assistant message `message_id`.
pub fn assistant_item(thread_id: &str, id: &str, message_id: &str, ordinal: u64, text: &str, streaming: bool) -> Value {
    written_as::<TurnItem>(json!({
        "id": id, "threadId": thread_id, "ordinal": ordinal, "status": if streaming { "running" } else { "completed" }, "startedAt": AT, "updatedAt": AT,
        "type": "assistant_message", "messageId": message_id, "text": text, "streaming": streaming,
    }))
}

/// A request for approval of a command, in `status`.
pub fn runtime_request(id: &str, status: &str) -> Value {
    written_as::<RuntimeRequest>(json!({
        "id": id, "nodeId": format!("node-of-{id}"), "kind": "command", "status": status, "responseCapability": { "type": "message" }, "createdAt": AT,
    }))
}
