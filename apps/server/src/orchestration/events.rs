//! Thread domain events: append to the global log and upsert the projection in
//! one transaction. Events carry whole records, so the projection is "latest
//! record per (kind, id)", with the thread record kept in `threads`.
//!
//! An event is typed (`domovoi_model::typed::DomainEvent`) from the moment it is
//! appended: `Store::append_event` takes one built from typed records. It is
//! serialized once, and its record's text is what the projection keeps. What
//! is read back is typed as well: a record by its id (`Store::record`), the
//! records of one kind (`Store::records`), a thread's outline, or all of it.

use super::stored::{self, PAYLOAD_KEY};
use crate::store::{Store, StoreError, StoredEvent, decode_record};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use domovoi_model::typed::{
    AppThread, Checkpoint, CheckpointScope, ContextHandoff, ContextTransfer, ConversationMessage, DomainEvent, EventBody, EventId, ExecutionNode,
    PlanArtifact, ProviderDriverKind, ProviderInstanceId, ProviderSession, ProviderThread, ProviderTurn, PullRequestLink, RecordKind, Run, RunAttempt,
    RuntimeRequest, StoredRecord, Subagent, Thread, ThreadChange, ThreadId, ThreadShell, Timestamp, TurnItem,
};

pub use domovoi_model::enc;

/// Provider attribution carried on provider-originated events.
#[derive(Debug, Clone, Default)]
pub struct Origin {
    pub driver: Option<String>,
    pub provider_instance_id: Option<String>,
    pub command_id: Option<String>,
}

/// A record kept as a row of its thread: one of a kind, read by its id or
/// with the others of its kind, and written by the event that carries its new version.
pub trait Kept: Serialize + DeserializeOwned + Clone {
    const KIND: RecordKind;

    /// The event that says this is the record's new version.
    fn updated(self) -> EventBody;

    /// The record a stored row of this kind holds; `None` for a row of another kind.
    fn of(stored: StoredRecord) -> Option<Arc<Self>>;
}

/// Declares the kinds of record kept: each with the event that carries its new version.
macro_rules! kept {
    ($($record:ty: $kind:ident => |$new:ident| $event:expr,)+) => {$(
        impl Kept for $record {
            const KIND: RecordKind = RecordKind::$kind;

            fn updated(self) -> EventBody {
                let $new = self;
                $event
            }

            fn of(stored: StoredRecord) -> Option<Arc<Self>> {
                match stored {
                    StoredRecord::$kind(record) => Some(record),
                    _ => None,
                }
            }
        }
    )+};
}

kept! {
    Run: Run => |run| EventBody::RunUpdated(Box::new(run)),
    RunAttempt: Attempt => |attempt| EventBody::RunAttemptUpdated(Box::new(attempt)),
    ExecutionNode: Node => |node| EventBody::NodeUpdated(Box::new(node)),
    Subagent: Subagent => |subagent| EventBody::SubagentUpdated(Box::new(subagent)),
    ProviderSession: ProviderSession => |session| EventBody::ProviderSessionUpdated(Box::new(session)),
    ProviderThread: ProviderThread => |provider_thread| EventBody::ProviderThreadUpdated(Box::new(provider_thread)),
    ProviderTurn: ProviderTurn => |turn| EventBody::ProviderTurnUpdated(Box::new(turn)),
    RuntimeRequest: RuntimeRequest => |request| EventBody::RuntimeRequestUpdated(Box::new(request)),
    ConversationMessage: Message => |message| EventBody::MessageUpdated(Box::new(message)),
    PlanArtifact: Plan => |plan| EventBody::PlanUpdated(Box::new(plan)),
    TurnItem: TurnItem => |item| EventBody::TurnItemUpdated(item),
    // A scope is written once; a checkpoint's new state is captured again.
    CheckpointScope: CheckpointScope => |scope| EventBody::CheckpointScopeCreated(Box::new(scope)),
    Checkpoint: Checkpoint => |checkpoint| EventBody::CheckpointCaptured(Box::new(checkpoint)),
    ContextHandoff: ContextHandoff => |handoff| EventBody::ContextHandoffUpdated(Box::new(handoff)),
    ContextTransfer: ContextTransfer => |transfer| EventBody::ContextTransferUpdated(Box::new(transfer)),
}

/// The id of the record an event carries: the thread's for its own events,
/// empty for an event that names no record of its own.
fn record_id(body: &EventBody) -> String {
    match body {
        EventBody::Thread { thread, .. } => thread.id.as_str().to_owned(),
        other => other.record().map(|record| record.id().to_owned()).unwrap_or_default(),
    }
}

fn event_id(thread_id: &str, body: &EventBody, origin: &Origin) -> EventId {
    let tail = crate::util::new_uuid();
    EventId::from(match &origin.command_id {
        Some(c) => format!("event:thread:{}:command:{}:{tail}", enc(thread_id), c),
        None => {
            let event_type = body.event_type();
            let kind = event_type.rsplit_once('.').map(|(k, _)| k).unwrap_or(event_type);
            format!("event:thread:{}:{kind}:{}:{tail}", enc(thread_id), enc(&record_id(body)))
        }
    })
}

/// The event that says `body` happened to the thread now, on `origin`'s behalf.
fn event_now(thread_id: &str, body: EventBody, origin: &Origin) -> DomainEvent {
    DomainEvent {
        id: event_id(thread_id, &body, origin),
        thread_id: ThreadId::from(thread_id),
        run_id: None,
        node_id: None,
        driver: origin.driver.as_deref().map(ProviderDriverKind::from),
        provider_instance_id: origin.provider_instance_id.as_deref().map(ProviderInstanceId::from),
        raw_event_id: None,
        occurred_at: Timestamp::from(domovoi_contracts::iso_now()),
        body,
    }
}

fn does_not_fit(event_type: &str, why: impl std::fmt::Display) -> StoreError {
    StoreError::Invalid(format!("the server built a `{event_type}` event the schema does not allow: {why}"))
}

/// An event and its JSON, serialized once. The record the event carries is
/// the last thing in that JSON, so its text is the projection's row as well.
struct Written {
    event: DomainEvent,
    text: String,
    /// Where the record starts in `text`.
    record_at: usize,
    command_id: Option<String>,
}

impl Written {
    fn new(event: DomainEvent, origin: &Origin) -> Result<Self, StoreError> {
        if matches!(event.body, EventBody::Unknown(_)) {
            return Err(does_not_fit(event.event_type(), "it has no event of that type"));
        }
        let text = serde_json::to_string(&event).map_err(|e| does_not_fit(event.event_type(), e))?;
        // Everything before the record is the envelope: strings only, in
        // which a quote is written `\"`. The first `,"payload":` is the key.
        let record_at = text.find(PAYLOAD_KEY).map(|at| at + PAYLOAD_KEY.len()).ok_or_else(|| does_not_fit(event.event_type(), "it carries no record"))?;
        Ok(Written { event, text, record_at, command_id: origin.command_id.clone() })
    }

    /// The JSON of the record the event carries.
    fn record(&self) -> &str {
        &self.text[self.record_at..self.text.len() - 1]
    }

    /// Inserts the event and applies it to the projection, inside `tx`.
    fn write(self, tx: &rusqlite::Connection, thread_id: &str) -> rusqlite::Result<StoredEvent> {
        let event_type = self.event.event_type().to_owned();
        let sequence =
            crate::store::insert_event(tx, "thread", thread_id, &event_type, &self.text, self.event.occurred_at.as_str(), self.command_id.as_deref())?;
        project(tx, thread_id, &self.event, self.record(), sequence as i64)?;
        Ok(StoredEvent { sequence, aggregate_kind: "thread".into(), aggregate_id: thread_id.to_owned(), event_type, thread_event: Some(self.event) })
    }
}

/// Every non-thread event moves `thread.updatedAt`; a provider thread that
/// belongs to this app thread and is no longer a queued placeholder becomes its
/// active provider thread (t3code ProjectionStore).
///
/// The record is edited where it is stored: it is not read, so what the
/// transaction has already written to it is kept, and nothing is taken from
/// the thread cache, which holds only what has committed. A record that is
/// not JSON stays as it is.
fn touch_thread(
    tx: &rusqlite::Connection,
    thread_id: &str,
    seq: i64,
    occurred_at: &Timestamp,
    provider_thread: Option<&ProviderThread>,
) -> rusqlite::Result<()> {
    let active = provider_thread.filter(|pt| pt.app_thread_id.as_ref().is_some_and(|id| id.as_str() == thread_id) && !pt.is_placeholder());
    match active {
        Some(pt) => tx
            .prepare_cached(
                "UPDATE threads SET record = json_set(record, '$.updatedAt', ?2, '$.activeProviderThreadId', ?4), last_sequence = ?3
                 WHERE id = ?1 AND json_valid(record)",
            )?
            .execute(params![thread_id, pt.updated_at.as_str(), seq, pt.id.as_str()])?,
        None => tx
            .prepare_cached("UPDATE threads SET record = json_set(record, '$.updatedAt', ?2), last_sequence = ?3 WHERE id = ?1 AND json_valid(record)")?
            .execute(params![thread_id, occurred_at.as_str(), seq])?,
    };
    Ok(())
}

/// Whether a record that streams is still streaming: `Some(false)` for the
/// final version of a message, or of a turn item that grows as it streams.
fn streaming(body: &EventBody) -> Option<bool> {
    match body {
        EventBody::MessageUpdated(message) => Some(message.streaming),
        EventBody::TurnItemUpdated(TurnItem::AssistantMessage(item)) => Some(item.streaming),
        EventBody::TurnItemUpdated(TurnItem::Reasoning(item)) => Some(item.streaming),
        EventBody::TurnItemUpdated(TurnItem::ProposedPlan(item)) => Some(item.streaming),
        _ => None,
    }
}

/// Once an entity's final version (`streaming: false`) is written, the
/// throttled flushes that led to it go from the log. Each carried the whole
/// text so far, so a long reply left hundreds of near-copies of itself; a
/// replay only ever needs the latest version, and nothing reads the log but
/// stream replays. Sequences are global, so a thread's events already skip
/// numbers; a client that saw a removed flush resumes after it and gets the
/// final version.
fn drop_superseded_flushes(tx: &rusqlite::Connection, tid: &str, kind: &str, event_type: &str, id: &str, seq: i64) -> rusqlite::Result<()> {
    // Its flushes are no older than the entity: the search covers the events
    // since it first appeared (one reply's), not the thread's whole log.
    let first: Option<i64> = tx
        .prepare_cached("SELECT first_sequence FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 AND entity_id = ?3")?
        .query_row(params![tid, kind, id], |r| r.get(0))
        .optional()?;
    let Some(first) = first else { return Ok(()) };
    tx.prepare_cached(
        "DELETE FROM events WHERE aggregate_kind = 'thread' AND aggregate_id = ?1 AND sequence >= ?5 AND sequence < ?2 AND type = ?3
           AND json_extract(payload, '$.payload.id') = ?4 AND json_extract(payload, '$.payload.streaming') = 1",
    )?
    .execute(params![tid, seq, event_type, id, first])?;
    Ok(())
}

/// The thread's events in `(after, through]` read on `c`. One that does not
/// decode is left out with a warning.
fn thread_events_on(c: &rusqlite::Connection, thread_id: &str, after: i64, through: i64) -> rusqlite::Result<Vec<StoredEvent>> {
    let mut st = c.prepare_cached(
        "SELECT sequence, type, payload FROM events
         WHERE aggregate_kind = 'thread' AND aggregate_id = ?1 AND sequence > ?2 AND sequence <= ?3
         ORDER BY sequence",
    )?;
    let mut rows = st.query(params![thread_id, after, through])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let event_type: String = row.get(1)?;
        let event = match stored::event(&event_type, row.get_ref(2)?.as_str()?) {
            Ok(event) => event,
            Err(e) => {
                tracing::warn!("skipping a stored event that does not decode: {e}");
                continue;
            }
        };
        events.push(StoredEvent {
            sequence: row.get::<_, i64>(0)? as u64,
            aggregate_kind: "thread".into(),
            aggregate_id: thread_id.to_owned(),
            event_type,
            thread_event: Some(event),
        });
    }
    Ok(events)
}

/// A thread as its stored rows hold it, read on `c` (one read transaction).
/// `None` if the thread has no record, or one that does not decode; a record
/// of its collections that does not decode is left out with a warning.
pub(super) fn load_thread(c: &rusqlite::Connection, thread_id: &str) -> rusqlite::Result<Option<Thread>> {
    let record: Option<String> =
        c.prepare_cached("SELECT record FROM threads WHERE id = ?1")?.query_row(params![thread_id], |r| r.get(0)).optional()?;
    let Some(record) = record.and_then(|r| decode_record::<AppThread>(&r, "thread record")) else { return Ok(None) };
    // Put in the order they first appeared here, not by the database: it
    // would copy every record's text to sort them.
    let mut st = c.prepare_cached("SELECT kind, record, first_sequence FROM thread_entities WHERE thread_id = ?1")?;
    let mut rows = st.query(params![thread_id])?;
    let mut records: Vec<(i64, StoredRecord)> = Vec::new();
    while let Some(row) = rows.next()? {
        let Some(kind) = RecordKind::of_collection(row.get_ref(0)?.as_str()?) else { continue };
        match stored::record(kind, row.get_ref(1)?.as_str()?) {
            Ok(record) => records.push((row.get(2)?, record)),
            Err(e) => tracing::warn!("skipping a stored thread entity that does not decode: {e}"),
        }
    }
    records.sort_by_key(|(first_sequence, _)| *first_sequence);
    let records = records.into_iter().map(|(_, record)| record);
    Ok(Some(Thread::from_records(record, records)))
}

/// Applies a thread event to the projection inside the event's transaction.
/// `record` is the JSON of what the event carries.
fn project(tx: &rusqlite::Connection, tid: &str, event: &DomainEvent, record: &str, seq: i64) -> rusqlite::Result<()> {
    if let EventBody::Thread { thread, .. } = &event.body {
        tx.prepare_cached(
            "INSERT INTO threads (id, project_id, record, last_sequence, deleted) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET record = excluded.record, last_sequence = excluded.last_sequence,
               deleted = excluded.deleted",
        )?
            .execute(params![tid, thread.project_id.as_str(), record, seq, thread.deleted_at.is_some()])?;
        return Ok(());
    }
    if let Some(reference) = event.body.record() {
        let (kind, id) = (reference.kind().collection(), reference.id());
        if streaming(&event.body) == Some(false) {
            drop_superseded_flushes(tx, tid, kind, event.event_type(), id, seq)?;
        }
        tx.prepare_cached(
            "INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id, kind, entity_id) DO UPDATE SET record = excluded.record",
        )?
            .execute(params![tid, kind, id, record, seq])?;
        let provider_thread = match &event.body {
            EventBody::ProviderThreadUpdated(provider_thread) => Some(&**provider_thread),
            _ => None,
        };
        return touch_thread(tx, tid, seq, &event.occurred_at, provider_thread);
    }
    match &event.body {
        EventBody::ProviderSessionDetached(detached) => {
            tx.prepare_cached("DELETE FROM thread_entities WHERE thread_id = ?1 AND kind = 'providerSessions' AND entity_id = ?2")?
                .execute(params![tid, detached.provider_session_id.as_str()])?;
        }
        // The run keeps its record; the work a restart cancelled is noted on it.
        EventBody::RunBackgroundWorkCancelled(cancelled) => {
            tx.prepare_cached(
                "UPDATE thread_entities
                 SET record = json_set(record, '$.restartCancelledBackgroundWork', json_extract(?3, '$.restartCancelledBackgroundWork'))
                 WHERE thread_id = ?1 AND kind = 'runs' AND entity_id = ?2 AND json_valid(record)",
            )?
                .execute(params![tid, cancelled.run_id.as_str(), record])?;
        }
        // The link to the same pull request (host, repository, number) is
        // replaced; the thread is otherwise as it was, `updatedAt` included.
        EventBody::ThreadPullRequestLinkUpdated(_) => return replace_pull_request_link(tx, tid, record, seq),
        _ => {}
    }
    touch_thread(tx, tid, seq, &event.occurred_at, None)
}

/// Puts `link` (JSON) in the place of the thread's link to the same pull request, if it has one.
fn replace_pull_request_link(tx: &rusqlite::Connection, tid: &str, link: &str, seq: i64) -> rusqlite::Result<()> {
    let stored: Option<String> =
        tx.prepare_cached("SELECT record FROM threads WHERE id = ?1")?.query_row(params![tid], |r| r.get(0)).optional()?;
    let thread = stored.and_then(|r| decode_record::<AppThread>(&r, "thread record"));
    let (Some(mut thread), Some(link)) = (thread, decode_record::<PullRequestLink>(link, "pull request link")) else {
        return Ok(());
    };
    let same = |held: &&mut PullRequestLink| held.host == link.host && held.repository == link.repository && held.number == link.number;
    if let Some(held) = thread.pull_requests.iter_mut().flatten().find(same) {
        *held = link;
        let record = serde_json::to_string(&thread).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        tx.prepare_cached("UPDATE threads SET record = ?2, last_sequence = ?3 WHERE id = ?1")?.execute(params![tid, record, seq])?;
    }
    Ok(())
}

/// What a command decides from: the thread's records apart from its messages
/// and turn items (the conversation's text: nearly all of a long thread's
/// bytes), each kind in the order its records first appeared
/// (`Store::thread_outline`). A message, a plan or the highest turn
/// item ordinal is read on its own: `Store::record`, `Store::last_item_ordinal`.
#[derive(Debug, Default)]
pub struct Outline {
    pub runs: Vec<Arc<Run>>,
    pub attempts: Vec<Arc<RunAttempt>>,
    pub nodes: Vec<Arc<ExecutionNode>>,
    pub subagents: Vec<Arc<Subagent>>,
    pub provider_threads: Vec<Arc<ProviderThread>>,
    pub provider_turns: Vec<Arc<ProviderTurn>>,
    pub runtime_requests: Vec<Arc<RuntimeRequest>>,
    pub checkpoint_scopes: Vec<Arc<CheckpointScope>>,
    pub checkpoints: Vec<Arc<Checkpoint>>,
    pub context_transfers: Vec<Arc<ContextTransfer>>,
}

impl Store {
    /// Appends an event built from typed records.
    pub fn append_event(&self, thread_id: &str, body: EventBody, origin: &Origin) -> Result<Arc<StoredEvent>, StoreError> {
        self.commit(thread_id, Written::new(event_now(thread_id, body, origin), origin)?)
    }

    /// Appends several events of one thread in one transaction (an import).
    /// If one cannot be written none is.
    pub fn append_events(&self, thread_id: &str, events: Vec<(EventBody, Origin)>) -> Result<(), StoreError> {
        let mut written = Vec::with_capacity(events.len());
        for (body, origin) in events {
            written.push(Written::new(event_now(thread_id, body, &origin), &origin)?);
        }
        self.db().with(|c| {
            let tx = c.savepoint()?;
            let mut stored = Vec::with_capacity(written.len());
            for event in written {
                stored.push(Arc::new(event.write(&tx, thread_id)?));
            }
            tx.commit()?;
            for e in &stored {
                self.publish(e);
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Writes the event and its projection in one transaction, then publishes it.
    fn commit(&self, thread_id: &str, written: Written) -> Result<Arc<StoredEvent>, StoreError> {
        Ok(self.db().with(|c| {
            let tx = c.savepoint()?;
            let stored = Arc::new(written.write(&tx, thread_id)?);
            tx.commit()?;
            self.publish(&stored);
            Ok(stored)
        })?)
    }

    /// A thread as its stored rows hold it, all read as of one moment.
    pub fn load_thread(&self, thread_id: &str) -> Result<Option<Thread>, StoreError> {
        Ok(self.db().read(|c| {
            // Inside a batch the batch's transaction is the one read in.
            if c.is_autocommit() {
                let tx = c.unchecked_transaction()?;
                load_thread(&tx, thread_id)
            } else {
                load_thread(c, thread_id)
            }
        })?)
    }

    /// The thread's sidebar shell (`projection::thread_shell`), from the
    /// thread cache: derived once per event however many ask for it.
    pub fn thread_shell(&self, thread_id: &str) -> Result<Option<Arc<ThreadShell>>, StoreError> {
        if self.db().in_batch() {
            // Uncommitted writes must not reach the cache: the batch may roll back.
            return Ok(super::projection::thread_shell(self, thread_id)?.map(Arc::new));
        }
        if !self.thread_cache().contains(thread_id) {
            // Loaded on a reader, in one read transaction: a long thread takes
            // milliseconds, and the writer is not held for them.
            let loaded = self.db().read(|c| {
                let tx = c.unchecked_transaction()?;
                let thread = load_thread(&tx, thread_id)?;
                let sequence: i64 = tx.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |r| r.get(0))?;
                Ok(thread.map(|t| (t, sequence)))
            })?;
            if let Some((mut thread, loaded_at)) = loaded {
                self.db().with(|c| {
                    // Under the writer lock: the thread's events committed since
                    // the load are applied, and none lands before the entry.
                    let sequence: i64 = c.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |r| r.get(0))?;
                    for event in thread_events_on(c, thread_id, loaded_at, sequence)? {
                        if let Some(event) = event.thread_event {
                            thread.apply(event);
                        }
                    }
                    self.thread_cache().insert(thread_id, thread, sequence as u64);
                    Ok(())
                })?;
            }
        }
        Ok(self.thread_cache().shell(thread_id))
    }

    /// Reads the record `id` of the thread, lets `change` edit it, and appends
    /// it as the event that carries its new version, all in one transaction:
    /// no other write lands between the read and the append, so a concurrent
    /// change to another field of the same record is never overwritten.
    /// Returns the record as written, or `None`, writing nothing, when the
    /// thread has no such record.
    pub fn update_record<R: Kept>(&self, thread_id: &str, id: &str, origin: &Origin, change: impl FnOnce(&mut R)) -> Result<Option<R>, StoreError> {
        self.update_record_if(thread_id, id, origin, |record| {
            change(record);
            true
        })
    }

    /// As `update_record`, for a change decided on the record as it is read:
    /// nothing is written when `change` returns `false`.
    pub fn update_record_if<R: Kept>(
        &self,
        thread_id: &str,
        id: &str,
        origin: &Origin,
        change: impl FnOnce(&mut R) -> bool,
    ) -> Result<Option<R>, StoreError> {
        self.db().batch(|| {
            let Some(mut record) = self.record::<R>(thread_id, id)? else { return Ok(None) };
            if !change(&mut record) {
                return Ok(None);
            }
            self.append_event(thread_id, record.clone().updated(), origin)?;
            Ok(Some(record))
        })
    }

    /// Reads the thread's own record, lets `edit` change it, and appends it as
    /// `change`, in one transaction (as `update_record` does for a record of
    /// the thread). Returns the record as written, or `None`, writing
    /// nothing, when the thread has no record or `edit` returns `false`.
    pub fn update_thread(
        &self,
        thread_id: &str,
        change: ThreadChange,
        origin: &Origin,
        edit: impl FnOnce(&mut AppThread) -> bool,
    ) -> Result<Option<AppThread>, StoreError> {
        self.db().batch(|| {
            let Some(mut thread) = self.app_thread(thread_id)? else { return Ok(None) };
            if !edit(&mut thread) {
                return Ok(None);
            }
            self.append_event(thread_id, EventBody::Thread { change, thread: Box::new(thread.clone()) }, origin)?;
            Ok(Some(thread))
        })
    }

    /// One record of a thread, typed. `None` if the thread has none with this
    /// id, or the stored one does not decode.
    pub fn record<R: Kept>(&self, thread_id: &str, id: &str) -> Result<Option<R>, StoreError> {
        let stored: Option<String> = self.db().read(|c| {
            c.prepare_cached("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 AND entity_id = ?3")?
                .query_row(params![thread_id, R::KIND.collection(), id], |r| r.get(0))
                .optional()
        })?;
        Ok(stored.and_then(|text| match stored::record(R::KIND, &text) {
            Ok(record) => R::of(record).map(Arc::unwrap_or_clone),
            Err(e) => {
                tracing::warn!("skipping a stored thread entity that does not decode: {e}");
                None
            }
        }))
    }

    /// Every record of one kind the thread has, in the order they first
    /// appeared, without loading the rest. One that does not decode is left
    /// out with a warning.
    pub fn records<R: Kept>(&self, thread_id: &str) -> Result<Vec<Arc<R>>, StoreError> {
        Ok(self.db().read(|c| {
            let mut st = c.prepare_cached("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 ORDER BY first_sequence")?;
            let mut rows = st.query(params![thread_id, R::KIND.collection()])?;
            let mut records = Vec::new();
            while let Some(row) = rows.next()? {
                match stored::record(R::KIND, row.get_ref(0)?.as_str()?) {
                    Ok(record) => records.extend(R::of(record)),
                    Err(e) => tracing::warn!("skipping a stored thread entity that does not decode: {e}"),
                }
            }
            Ok(records)
        })?)
    }

    /// The highest ordinal among the thread's turn items in `ordinals`.
    pub fn last_item_ordinal(&self, thread_id: &str, ordinals: std::ops::Range<u64>) -> Result<Option<u64>, StoreError> {
        Ok(self.db().read(|c| {
            c.prepare_cached(
                "SELECT MAX(json_extract(record, '$.ordinal')) FROM thread_entities
                 WHERE thread_id = ?1 AND kind = 'turnItems' AND json_extract(record, '$.ordinal') >= ?2 AND json_extract(record, '$.ordinal') < ?3",
            )?
            .query_row(params![thread_id, ordinals.start as i64, ordinals.end as i64], |r| r.get::<_, Option<i64>>(0))
        })?
        .map(|ordinal| ordinal as u64))
    }

    /// The thread's own record, typed. `None` if the thread has none, or the
    /// stored one does not decode.
    pub fn app_thread(&self, thread_id: &str) -> Result<Option<AppThread>, StoreError> {
        Ok(self.stored_thread_record(thread_id)?.and_then(|r| decode_record::<AppThread>(&r, "thread record")))
    }

    /// Every live thread's own record, typed, oldest first. One that does not
    /// decode is left out with a warning.
    pub fn app_threads(&self) -> Result<Vec<AppThread>, StoreError> {
        Ok(self.db().read(|c| {
            let mut st = c.prepare_cached("SELECT record FROM threads WHERE deleted = 0 ORDER BY rowid")?;
            let mut rows = st.query([])?;
            let mut threads = Vec::new();
            while let Some(row) = rows.next()? {
                threads.extend(decode_record::<AppThread>(row.get_ref(0)?.as_str()?, "thread record"));
            }
            Ok(threads)
        })?)
    }

    fn stored_thread_record(&self, thread_id: &str) -> Result<Option<String>, StoreError> {
        Ok(self.db().read(|c| c.prepare_cached("SELECT record FROM threads WHERE id = ?1")?.query_row(params![thread_id], |r| r.get(0)).optional())?)
    }

    /// Every live thread's id with the sequence of its last event, oldest first.
    pub fn thread_ids(&self) -> Result<Vec<(String, u64)>, StoreError> {
        Ok(self.db().read(|c| {
            let mut st = c.prepare_cached("SELECT id, last_sequence FROM threads WHERE deleted = 0 ORDER BY rowid")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
            rows.collect()
        })?)
    }

    /// The thread's shell for a snapshot of every thread, `last_sequence`
    /// being its last event's (`thread_ids`). A shell is remembered until the
    /// thread's next event: only the threads that changed since the last
    /// snapshot are read again. The thread cache is left to the open threads.
    pub fn snapshot_shell(&self, thread_id: &str, last_sequence: u64) -> Result<Option<Arc<ThreadShell>>, StoreError> {
        if let Some((_, shell)) = crate::util::locked(self.snapshot_shells()).get(thread_id).filter(|(at, _)| *at == last_sequence) {
            return Ok(Some(shell.clone()));
        }
        let shell = if self.thread_cache().contains(thread_id) {
            self.thread_shell(thread_id)?
        } else {
            super::projection::thread_shell(self, thread_id)?.map(Arc::new)
        };
        // Read after `last_sequence`, so no older than it: if the thread has
        // moved on since, the next snapshot's sequence differs and reads it again.
        if let Some(shell) = &shell {
            crate::util::locked(self.snapshot_shells()).insert(thread_id.to_owned(), (last_sequence, shell.clone()));
        }
        Ok(shell)
    }

    /// The thread's records apart from its messages and turn items: what a
    /// command decides from, read without the conversation's text. A record
    /// that does not decode is left out with a warning.
    pub fn thread_outline(&self, thread_id: &str) -> Result<Outline, StoreError> {
        Ok(self.db().read(|c| {
            let mut st = c.prepare_cached(
                "SELECT kind, record FROM thread_entities WHERE thread_id = ?1 AND kind IN
                    ('runs', 'attempts', 'nodes', 'subagents', 'providerThreads', 'providerTurns',
                     'runtimeRequests', 'checkpointScopes', 'checkpoints', 'contextTransfers')
                 ORDER BY first_sequence",
            )?;
            let mut rows = st.query(params![thread_id])?;
            let mut outline = Outline::default();
            while let Some(row) = rows.next()? {
                let Some(kind) = RecordKind::of_collection(row.get_ref(0)?.as_str()?) else { continue };
                match stored::record(kind, row.get_ref(1)?.as_str()?) {
                    Ok(StoredRecord::Run(record)) => outline.runs.push(record),
                    Ok(StoredRecord::Attempt(record)) => outline.attempts.push(record),
                    Ok(StoredRecord::Node(record)) => outline.nodes.push(record),
                    Ok(StoredRecord::Subagent(record)) => outline.subagents.push(record),
                    Ok(StoredRecord::ProviderThread(record)) => outline.provider_threads.push(record),
                    Ok(StoredRecord::ProviderTurn(record)) => outline.provider_turns.push(record),
                    Ok(StoredRecord::RuntimeRequest(record)) => outline.runtime_requests.push(record),
                    Ok(StoredRecord::CheckpointScope(record)) => outline.checkpoint_scopes.push(record),
                    Ok(StoredRecord::Checkpoint(record)) => outline.checkpoints.push(record),
                    Ok(StoredRecord::ContextTransfer(record)) => outline.context_transfers.push(record),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("skipping a stored thread entity that does not decode: {e}"),
                }
            }
            Ok(outline)
        })?)
    }

    /// Whether the thread has any entity of `kind`.
    pub fn has_entities(&self, thread_id: &str, kind: &str) -> Result<bool, StoreError> {
        Ok(self.db().read(|c| {
            c.prepare_cached("SELECT EXISTS (SELECT 1 FROM thread_entities WHERE thread_id = ?1 AND kind = ?2)")?
                .query_row(params![thread_id, kind], |r| r.get(0))
        })?)
    }

    /// The thread's events in `(after, through]`, read through the
    /// `events_aggregate` index (not by filtering the whole range).
    pub fn thread_events_after(&self, thread_id: &str, after: u64, through: u64) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(self.db().read(|c| thread_events_on(c, thread_id, after as i64, through as i64))?)
    }

    /// What the request `command_id` was answered with, if it was carried out
    /// before. Fails if that answer is not a `T`: the id was used by a request
    /// of another kind (a launch's by a command, or the other way round).
    pub fn command_receipt<T: DeserializeOwned>(&self, command_id: &str) -> Result<Option<T>, StoreError> {
        let stored: Option<String> = self
            .db()
            .read(|c| c.prepare_cached("SELECT result FROM command_receipts WHERE command_id = ?1")?.query_row(params![command_id], |r| r.get(0)).optional())?;
        match stored {
            None => Ok(None),
            Some(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| StoreError::Invalid(format!("Command id {command_id} was already used by a request of another kind."))),
        }
    }

    pub fn put_command_receipt<T: Serialize>(&self, command_id: &str, result: &T) -> Result<(), StoreError> {
        let text = serde_json::to_string(result).map_err(|e| StoreError::Invalid(format!("the answer to {command_id} does not encode: {e}")))?;
        self.db().with(|c| {
            c.prepare_cached("INSERT OR REPLACE INTO command_receipts (command_id, result, created_at) VALUES (?1, ?2, ?3)")?
                .execute(params![command_id, text, domovoi_contracts::iso_now()])
                .map(|_| ())
        })?;
        Ok(())
    }
}

impl Store {
    /// Keeps an effect until `outbox_remove` (replaces one with the same key).
    pub fn outbox_put<T: Serialize>(&self, key: &str, effect: &T) -> Result<(), StoreError> {
        let text = serde_json::to_string(effect).map_err(|e| StoreError::Invalid(format!("the effect {key} does not encode: {e}")))?;
        self.db().with(|c| c.execute("INSERT OR REPLACE INTO effect_outbox (key, effect, created_at) VALUES (?1, ?2, ?3)",
            params![key, text, domovoi_contracts::iso_now()]).map(|_| ()))?;
        Ok(())
    }

    pub fn outbox_remove(&self, key: &str) -> Result<(), StoreError> {
        self.db().with(|c| c.execute("DELETE FROM effect_outbox WHERE key = ?1", params![key]).map(|_| ()))?;
        Ok(())
    }

    /// Every effect still to do, oldest first. One that no longer decodes is
    /// left out with a warning.
    pub fn outbox_all<T: DeserializeOwned>(&self) -> Result<Vec<T>, StoreError> {
        Ok(self.db().read(|c| {
            let mut st = c.prepare_cached("SELECT effect FROM effect_outbox ORDER BY created_at, rowid")?;
            let mut rows = st.query([])?;
            let mut effects = Vec::new();
            while let Some(row) = rows.next()? {
                effects.extend(decode_record::<T>(row.get_ref(0)?.as_str()?, "outbox effect"));
            }
            Ok(effects)
        })?)
    }

    /// Event-log maintenance (t3code ProjectionMaintenance): drops the events
    /// and entities of threads deleted before `deleted_before`, keeping their
    /// thread record as a tombstone, and command receipts older than
    /// `receipts_before` (or with no time, from before receipts had one).
    /// Returns (events, entities, receipts) removed.
    pub fn prune(&self, deleted_before: &str, receipts_before: &str) -> Result<(usize, usize, usize), StoreError> {
        Ok(self.db().with(|c| {
            let tx = c.savepoint()?;
            // A record that is not JSON, or says no time, is never old enough.
            let ids: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT id FROM threads WHERE deleted = 1 AND json_valid(record)
                       AND json_type(record, '$.deletedAt') = 'text' AND json_extract(record, '$.deletedAt') < ?1",
                )?;
                let rows = st.query_map(params![deleted_before], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let (mut events, mut entities) = (0, 0);
            for id in &ids {
                events += tx.execute("DELETE FROM events WHERE aggregate_kind = 'thread' AND aggregate_id = ?1", params![id])?;
                entities += tx.execute("DELETE FROM thread_entities WHERE thread_id = ?1", params![id])?;
            }
            let receipts = tx.execute("DELETE FROM command_receipts WHERE created_at IS NULL OR created_at < ?1", params![receipts_before])?;
            tx.commit()?;
            self.thread_cache().remove(&ids);
            crate::util::locked(self.snapshot_shells()).retain(|id, _| !ids.contains(id));
            Ok((events, entities, receipts))
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::test_records::{append, assistant_item, event_body, message, node, run, stored_thread, thread, with};
    use serde_json::{Value, json};
    use domovoi_model::typed::{ConversationMessage, DispatchCommandResult};

    /// The message an event carries.
    fn message_of(event: &StoredEvent) -> Option<&ConversationMessage> {
        match &event.thread_event.as_ref()?.body {
            EventBody::MessageUpdated(message) => Some(message),
            _ => None,
        }
    }

    /// A thread is loaded for the cache without holding the writer, so
    /// events land meanwhile: the entry still has every one of them.
    #[test]
    fn a_thread_loaded_while_events_land_misses_none() {
        let dir = std::env::temp_dir().join(format!("domovoi-cache-load-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(crate::db::Db::open(&dir.join("state.sqlite")).unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        // The writer stops between events when asked, so the reader can compare with nothing in flight.
        use std::sync::atomic::{AtomicBool, Ordering};
        let (pause, parked) = (std::sync::Arc::new(AtomicBool::new(false)), std::sync::Arc::new(AtomicBool::new(false)));
        let writer = {
            let (store, pause, parked) = (store.clone(), pause.clone(), parked.clone());
            std::thread::spawn(move || {
                for i in 0..1500 {
                    while pause.load(Ordering::SeqCst) {
                        parked.store(true, Ordering::SeqCst);
                        std::thread::yield_now();
                    }
                    parked.store(false, Ordering::SeqCst);
                    append(&store, "t1", "message.updated", message("t1", &format!("m{i}"), "user", &"x".repeat(2000), false), &Origin::default()).unwrap();
                }
            })
        };
        let mut loads = 0;
        while !writer.is_finished() {
            store.thread_cache().remove(&["t1".to_owned()]);
            let _ = store.thread_shell("t1").unwrap();
            pause.store(true, Ordering::SeqCst);
            while !parked.load(Ordering::SeqCst) && !writer.is_finished() {
                std::thread::yield_now();
            }
            let cached = store.thread_shell("t1").unwrap().unwrap();
            let stored = crate::orchestration::projection::thread_shell(&store, "t1").unwrap().unwrap();
            assert_eq!(*cached, stored, "the cached thread has every event (load {loads})");
            loads += 1;
            pause.store(false, Ordering::SeqCst);
            while parked.load(Ordering::SeqCst) && !writer.is_finished() {
                std::thread::yield_now();
            }
        }
        writer.join().unwrap();
        assert!(loads > 5, "the thread was loaded while events landed ({loads} loads)");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_snapshot_reads_again_only_the_threads_that_changed() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let o = Origin::default();
        for id in ["t1", "t2"] {
            append(&store, id, "thread.created", with(thread(id, "p"), json!({ "title": "Old" })), &o).unwrap();
        }
        let shells = |store: &Store| -> Vec<Arc<ThreadShell>> {
            store.thread_ids().unwrap().into_iter().map(|(id, at)| store.snapshot_shell(&id, at).unwrap().unwrap()).collect()
        };
        let first = shells(&store);
        append(&store, "t2", "thread.metadata-updated", with(thread("t2", "p"), json!({ "title": "New" })), &o).unwrap();
        append(&store, "t2", "message.updated", message("t2", "m1", "user", "hi", false), &o).unwrap();
        let second = shells(&store);
        assert!(Arc::ptr_eq(&first[0], &second[0]), "the unchanged thread's shell is the one read before");
        assert_eq!((first[1].title.as_str(), second[1].title.as_str()), ("Old", "New"), "the changed thread is read again");
        assert_eq!(*second[1], crate::orchestration::projection::thread_shell(&store, "t2").unwrap().unwrap(), "and matches its projection, the message's event included");
    }

    #[test]
    fn a_finished_reply_leaves_only_its_final_version_in_the_log() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        let msg = |text: &str, streaming: bool| message("t1", "m1", "assistant", text, streaming);
        let first = append(&store, "t1", "message.updated", msg("a", true), &o).unwrap().sequence;
        append(&store, "t1", "message.updated", message("t1", "m2", "assistant", "other", true), &o).unwrap();
        append(&store, "t1", "message.updated", msg("ab", true), &o).unwrap();
        let last = append(&store, "t1", "message.updated", msg("abc", false), &o).unwrap().sequence;
        let replay = store.thread_events_after("t1", 0, last).unwrap();
        let m1: Vec<Value> = replay.iter().filter_map(message_of).filter(|m| m.id.as_str() == "m1").map(|m| json!(m)).collect();
        assert_eq!(m1, [msg("abc", false)], "one version of the finished reply");
        assert!(replay.iter().filter_map(message_of).any(|m| m.id.as_str() == "m2"), "another reply still streaming keeps its flush");
        // A client that saw the first flush resumes after it and gets the final text.
        let resumed = store.thread_events_after("t1", first, last).unwrap();
        assert_eq!(resumed.last().and_then(message_of).unwrap().text, "abc");
        assert_eq!(store.record::<ConversationMessage>("t1", "m1").unwrap().unwrap().text, "abc");
    }

    /// A streamed turn item's flushes go the same way once its final version is written.
    #[test]
    fn a_finished_turn_item_leaves_only_its_final_version_in_the_log() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        for (text, streaming) in [("a", true), ("ab", true), ("abc", false)] {
            append(&store, "t1", "turn-item.updated", assistant_item("t1", "i1", "m1", 1, text, streaming), &o).unwrap();
        }
        let replay = store.thread_events_after("t1", 0, 99).unwrap();
        assert_eq!(replay.iter().filter(|e| e.event_type == "turn-item.updated").count(), 1);
    }

    #[test]
    fn pruning_drops_long_deleted_threads_and_old_receipts_only() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "old", "thread.created", thread("old", "p"), &o).unwrap();
        append(&store, "old", "run.created", run("old", "r1", "queued"), &o).unwrap();
        append(&store, "old", "thread.deleted", with(thread("old", "p"), json!({ "deletedAt": "2020-01-01T00:00:00.000Z" })), &o).unwrap();
        append(&store, "new", "thread.created", thread("new", "p"), &o).unwrap();
        append(&store, "new", "thread.deleted", with(thread("new", "p"), json!({ "deletedAt": "2099-01-01T00:00:00.000Z" })), &o).unwrap();
        append(&store, "live", "thread.created", thread("live", "p"), &o).unwrap();
        store.put_command_receipt("c1", &DispatchCommandResult { sequence: 1 }).unwrap();
        let (events, entities, receipts) = store.prune("2025-01-01T00:00:00.000Z", "2000-01-01T00:00:00.000Z").unwrap();
        assert_eq!((events, entities, receipts), (3, 1, 0));
        assert!(store.app_thread("old").unwrap().is_some_and(|t| t.deleted_at.is_some()), "the tombstone stays");
        assert_eq!(store.thread_events_after("new", 0, 99).unwrap().len(), 2, "deleted too recently");
        assert_eq!(store.thread_events_after("live", 0, 99).unwrap().len(), 1);
        assert_eq!(store.prune("2025-01-01T00:00:00.000Z", "2100-01-01T00:00:00.000Z").unwrap().2, 1, "old receipts go");
        assert!(store.command_receipt::<DispatchCommandResult>("c1").unwrap().is_none());
        store.outbox_put("k", &("an effect", 1)).unwrap();
        store.outbox_put("k", &("an effect", 2)).unwrap();
        store.outbox_put("other", &"not one of those").unwrap();
        assert_eq!(store.outbox_all::<(String, u64)>().unwrap(), vec![("an effect".to_owned(), 2)], "the one that does not decode is left out");
        store.outbox_remove("k").unwrap();
        assert!(store.outbox_all::<(String, u64)>().unwrap().is_empty());
    }

    /// A record's new version goes into its own kind's collection, under its id.
    #[test]
    fn a_kept_record_is_written_to_its_kinds_collection() {
        fn kept<R: Kept>(event_type: &str, payload: Value) {
            let EventBody::Thread { .. } = event_body("t1", "thread.created", thread("t1", "p")) else { panic!("a thread event") };
            let record: R = serde_json::from_value(payload).expect("the record");
            let body = record.updated();
            assert_eq!(body.event_type(), event_type);
            assert_eq!(body.record().map(|record| record.kind()), Some(R::KIND), "{event_type}");
        }
        kept::<Run>("run.updated", run("t1", "r1", "queued"));
        kept::<ExecutionNode>("node.updated", node("t1", "n1", "running"));
        kept::<ConversationMessage>("message.updated", message("t1", "m1", "user", "hi", false));
        kept::<TurnItem>("turn-item.updated", assistant_item("t1", "i1", "m1", 1, "hi", false));
        kept::<RuntimeRequest>("runtime-request.updated", crate::test_records::runtime_request("q1", "pending"));
    }

    #[test]
    fn projection_keeps_latest_record_in_first_seen_order() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        append(&store, "t1", "run.created", run("t1", "r1", "queued"), &o).unwrap();
        append(&store, "t1", "message.updated", message("t1", "m1", "user", "hi", false), &o).unwrap();
        append(&store, "t1", "run.updated", run("t1", "r1", "running"), &o).unwrap();
        let runs = store.records::<Run>("t1").unwrap();
        assert_eq!(json!(runs), json!([run("t1", "r1", "running")]));
        assert_eq!(store.records::<ConversationMessage>("t1").unwrap().len(), 1);
        assert_eq!(store.thread_outline("t1").unwrap().runs, runs);
        assert_eq!(store.thread_events_after("t1", 0, 99).unwrap().len(), 4);
        let ev = store.thread_events_after("t1", 3, 4).unwrap().remove(0).thread_event.unwrap();
        assert_eq!(ev.event_type(), "run.updated");
        assert!(ev.id.as_str().starts_with("event:thread:t1:run:r1:"));
    }

    /// An event is written once: the log's row is the event as subscribers
    /// get it, and the projection's row is the record inside it, to the byte.
    #[test]
    fn the_log_and_the_projection_hold_the_same_text_of_a_record() {
        let store = Store::new(Db::open_in_memory().unwrap());
        // A thread id that looks like the start of a record must not be taken for one.
        let tid = r#"t,"payload":{"#;
        let o = Origin { driver: Some("codex".into()), provider_instance_id: Some("codex".into()), command_id: None };
        append(&store, tid, "thread.created", thread(tid, "p"), &o).unwrap();
        let stored = append(&store, tid, "message.updated", message(tid, "m1", "assistant", "say \"hi\" {}", false), &o).unwrap();
        let (logged, kept): (String, String) = store
            .db()
            .read(|c| {
                c.query_row(
                    "SELECT e.payload, m.record FROM events e, thread_entities m WHERE e.sequence = ?1 AND m.thread_id = ?2 AND m.entity_id = 'm1'",
                    params![stored.sequence as i64, tid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .unwrap();
        let event = stored.thread_event.as_ref().unwrap();
        assert_eq!(logged, serde_json::to_string(event).unwrap());
        assert_eq!(kept, serde_json::to_string(message_of(&stored).unwrap()).unwrap());
        assert!(logged.ends_with(&format!("{kept}}}")));
        assert_eq!(serde_json::from_str::<DomainEvent>(&logged).unwrap(), *event, "and reads back as the event it was");
    }

    /// The thread record is edited where it is stored: an event of the
    /// thread leaves every other byte of it as it was.
    #[test]
    fn an_event_moves_only_the_threads_updated_at() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        let record = with(thread("t1", "p"), json!({ "title": "Tabs\t, \"quotes\", é and \u{1F600} \\ / </script>", "branch": "a/b" }));
        append(&store, "t1", "thread.created", record, &o).unwrap();
        let text = |store: &Store| store.db().read(|c| c.query_row("SELECT record FROM threads WHERE id = 't1'", [], |r| r.get::<_, String>(0))).unwrap();
        let before = text(&store);
        let stored = append(&store, "t1", "message.updated", message("t1", "m1", "user", "hi", false), &o).unwrap();
        let occurred_at = stored.thread_event.as_ref().unwrap().occurred_at.as_str().to_owned();
        let expected = before.replace(&format!("\"updatedAt\":\"{}\"", crate::test_records::AT), &format!("\"updatedAt\":\"{occurred_at}\""));
        assert_ne!(expected, before);
        assert_eq!(text(&store), expected);
        assert_eq!(store.thread_ids().unwrap(), [("t1".to_owned(), stored.sequence)], "and the thread's last event is this one");
    }

    /// What the store keeps for a thread is what the thread held in memory
    /// becomes, for the events that do more than replace a record.
    #[test]
    fn the_projection_and_the_cached_thread_agree_on_events_that_edit_records() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        let link = json!({ "host": "github.com", "repository": "o/r", "number": 7, "url": "https://github.com/o/r/pull/7", "source": "manual",
            "linkedAt": crate::test_records::AT, "snapshot": null, "stack": null });
        append(&store, "t1", "thread.created", with(thread("t1", "p"), json!({ "pullRequests": [link] })), &o).unwrap();
        append(&store, "t1", "run.created", run("t1", "r1", "running"), &o).unwrap();
        let _ = store.thread_shell("t1").unwrap(); // the thread is cached
        let same = |what: &str| {
            let cached = store.thread_shell("t1").unwrap().unwrap();
            assert_eq!(*cached, crate::orchestration::projection::thread_shell(&store, "t1").unwrap().unwrap(), "after {what}");
        };
        append(&store, "t1", "run.background-work-cancelled", json!({ "runId": "r1", "restartCancelledBackgroundWork": [{ "kind": "shell", "label": "make" }] }), &o).unwrap();
        same("cancelled background work");
        assert_eq!(json!(store.record::<Run>("t1", "r1").unwrap().unwrap().restart_cancelled_background_work), json!([{ "kind": "shell", "label": "make" }]));
        let relinked = with(link.clone(), json!({ "source": "agent" }));
        append(&store, "t1", "thread.pull-request-link-updated", relinked, &o).unwrap();
        same("a pull request link's new state");
        assert_eq!(stored_thread(&store, "t1").unwrap()["pullRequests"][0]["source"], "agent");
        let other = with(link, json!({ "number": 8, "source": "agent" }));
        append(&store, "t1", "thread.pull-request-link-updated", other, &o).unwrap();
        same("a link the thread does not have");
        assert_eq!(store.app_thread("t1").unwrap().unwrap().pull_requests.map(|links| links.len()), Some(1));
        let detached = json!({ "providerSessionId": "s1", "detachedAt": crate::test_records::AT });
        append(&store, "t1", "provider-session.detached", detached, &o).unwrap();
        same("a detached session");
    }

    /// An event of a type the schema lacks is refused; of several events
    /// written together, either all are or none is.
    #[test]
    fn an_event_the_schema_lacks_is_refused_with_all_written_with_it() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        let before = store.latest_sequence().unwrap();
        // A type of a newer server, as it would be read from JSON.
        let unknown = || {
            let event = json!({ "id": "e", "threadId": "t1", "occurredAt": crate::test_records::AT, "type": "run.started", "payload": run("t1", "r1", "running") });
            DomainEvent::from_value(event).unwrap().body
        };
        assert!(matches!(store.append_event("t1", unknown(), &o), Err(StoreError::Invalid(why)) if why.contains("no event of that type")));
        let both = vec![(event_body("t1", "run.created", run("t1", "r1", "queued")), Origin::default()), (unknown(), Origin::default())];
        assert!(matches!(store.append_events("t1", both), Err(StoreError::Invalid(_))));
        assert_eq!(store.latest_sequence().unwrap(), before);
        assert!(store.records::<Run>("t1").unwrap().is_empty());
        // An edit that is not made writes nothing.
        let left = store.update_record_if("t1", "no-such-run", &o, |_: &mut Run| true).unwrap();
        assert!(left.is_none() && store.latest_sequence().unwrap() == before);
        let both = vec![(event_body("t1", "run.created", run("t1", "r1", "queued")), Origin::default()), (event_body("t1", "run.updated", run("t1", "r1", "running")), Origin::default())];
        store.append_events("t1", both).unwrap();
        assert_eq!(store.latest_sequence().unwrap(), before + 2);
        let kept = store.update_record_if("t1", "r1", &o, |_: &mut Run| false).unwrap();
        assert!(kept.is_none() && store.latest_sequence().unwrap() == before + 2, "an edit declined writes nothing");
    }

    /// A file change written by a server that kept its patch as `diff` is
    /// read with the patch where the schema has it, however it is read.
    #[test]
    fn a_stored_file_change_keeps_the_patch_it_was_written_with() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new";
        let item = |id: &str, fields: Value| {
            with(json!({ "id": id, "threadId": "t1", "ordinal": 1, "status": "completed", "updatedAt": crate::test_records::AT,
                "type": "file_change", "fileName": "a.txt" }), fields)
        };
        // As an older server left them: the record and its event, each with `diff`.
        let stored = [
            item("old", json!({ "diff": patch })),
            item("both", json!({ "diff": "stale", "diffStr": patch })),
            item("mention", json!({ "type": "system_notice", "message": "see \"diff\": above", "diff": "not a patch of this type" })),
        ];
        store.db().with(|c| {
            for (at, record) in stored.iter().enumerate() {
                let event = json!({ "id": format!("e{at}"), "threadId": "t1", "occurredAt": crate::test_records::AT, "type": "turn-item.updated", "payload": record });
                c.execute("INSERT INTO events (aggregate_kind, aggregate_id, type, payload, occurred_at) VALUES ('thread', 't1', 'turn-item.updated', ?1, 'x')", params![event.to_string()])?;
                c.execute(
                    "INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence) VALUES ('t1', 'turnItems', ?1, ?2, ?3)",
                    params![record["id"].as_str(), record.to_string(), 10 + at as i64],
                )?;
            }
            Ok(())
        }).unwrap();

        let patch_of = |item: &TurnItem| match item {
            TurnItem::FileChange(change) => change.diff_str.clone(),
            _ => None,
        };
        let loaded = store.load_thread("t1").unwrap().unwrap();
        assert_eq!(loaded.turn_items.len(), 3);
        assert_eq!(patch_of(loaded.turn_items.get("old").unwrap()).as_deref(), Some(patch));
        assert_eq!(patch_of(loaded.turn_items.get("both").unwrap()).as_deref(), Some(patch), "a patch already in its place stays");
        let replayed: Vec<Option<String>> = store.thread_events_after("t1", 0, 99).unwrap().iter().filter_map(|e| match &e.thread_event.as_ref()?.body {
            EventBody::TurnItemUpdated(item) => Some(patch_of(item)),
            _ => None,
        }).collect();
        assert_eq!(replayed, [Some(patch.to_owned()), Some(patch.to_owned()), None]);
        // Read by its id, or with the others of its kind.
        let by_id = store.record::<TurnItem>("t1", "old").unwrap().unwrap();
        let by_kind = store.records::<TurnItem>("t1").unwrap();
        assert_eq!(patch_of(&by_id).as_deref(), Some(patch));
        assert_eq!(by_kind.iter().map(|item| patch_of(item)).collect::<Vec<_>>(), [Some(patch.to_owned()), Some(patch.to_owned()), None]);
        // An item of another type is not taken for a file change.
        assert!(matches!(&*by_kind[2], TurnItem::SystemNotice(notice) if notice.message == "see \"diff\": above"));
        // And such a record is written again with its patch where the schema has it.
        store.update_record("t1", "old", &o, |item: &mut TurnItem| item.set_status(domovoi_model::typed::TurnItemStatus::Failed)).unwrap().unwrap();
        let rewritten = crate::test_records::stored(&store, "t1", RecordKind::TurnItem, "old");
        assert_eq!((rewritten["diffStr"].as_str(), rewritten.get("diff"), rewritten["status"].as_str()), (Some(patch), None, Some("failed")));
    }

    /// A stored row that does not decode is left out, never fatal; a thread
    /// whose own record does not decode has no shell and no projection.
    #[test]
    fn stored_rows_that_do_not_decode_are_left_out() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let o = Origin::default();
        for id in ["t1", "t2"] {
            append(&store, id, "thread.created", thread(id, "p"), &o).unwrap();
            append(&store, id, "message.updated", message(id, "m1", "user", "hi", false), &o).unwrap();
        }
        store.db().with(|c| {
            c.execute("INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence) VALUES ('t1', 'messages', 'bad', '{\"id\":\"bad\"}', 9)", [])?;
            c.execute("INSERT INTO events (aggregate_kind, aggregate_id, type, payload, occurred_at) VALUES ('thread', 't1', 'message.updated', '{\"type\":\"message.updated\"}', 'x')", [])?;
            c.execute("UPDATE threads SET record = '{\"id\":\"t2\"}' WHERE id = 't2'", [])
        }).unwrap();
        let loaded = store.load_thread("t1").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(store.thread_events_after("t1", 0, 99).unwrap().len(), 2);
        assert!(store.thread_shell("t1").unwrap().is_some());
        assert!(store.load_thread("t2").unwrap().is_none());
        assert!(store.thread_shell("t2").unwrap().is_none());
        // An event of the thread still lands, and leaves its record as it is.
        append(&store, "t2", "message.updated", message("t2", "m2", "user", "more", false), &o).unwrap();
        assert_eq!(stored_thread(&store, "t2").unwrap(), json!({ "id": "t2", "updatedAt": store.thread_events_after("t2", 0, 99).unwrap().last().unwrap().thread_event.as_ref().unwrap().occurred_at }));
    }
}
