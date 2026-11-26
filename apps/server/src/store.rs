//! Application event log + projections. Every state change is an event with a
//! global sequence; projections are updated in the same transaction (as in
//! t3code's EventSink), then the event is broadcast to live subscribers.

use crate::db::Db;
use rusqlite::{OptionalExtension, params};
use std::sync::Arc;
use domovoi_contracts::shell::ProjectShell;
use domovoi_model::typed::{DomainEvent, ThreadShell};
use tokio::sync::broadcast;

/// An event as the log holds it: its place in the log, what it changed, and
/// for a thread's event what it says.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence: u64,
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub event_type: String,
    /// A thread's event. `None` for a project's, whose new state is read from the store.
    pub thread_event: Option<DomainEvent>,
    /// For an event that carries a turn item's new version: how long the
    /// item's growing text was, when the new version is the last one with
    /// more text and nothing else changed. A subscriber that has the last
    /// version is then sent the text added, not the item again.
    pub grew_from: Option<usize>,
}

/// Where an event is in the log and what it changed, without what it says.
#[derive(Debug, Clone)]
pub struct EventHead {
    pub sequence: u64,
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub event_type: String,
}

#[derive(Clone)]
pub struct Store {
    db: Db,
    /// Shared, not copied, by every subscriber.
    tx: broadcast::Sender<Arc<StoredEvent>>,
    /// Recently used threads, kept current by every thread event.
    threads: Arc<crate::orchestration::thread_cache::ThreadCache>,
    /// Each thread's shell as the last snapshot of all threads read it, with
    /// the sequence of the thread's last event then.
    snapshot_shells: Arc<SnapshotShells>,
}

type SnapshotShells = std::sync::Mutex<std::collections::HashMap<String, (u64, Arc<ThreadShell>)>>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

impl Store {
    pub fn new(db: Db) -> Self {
        let (tx, _) = broadcast::channel(4096);
        Store { db, tx, threads: Arc::default(), snapshot_shells: Arc::default() }
    }

    pub(crate) fn thread_cache(&self) -> &crate::orchestration::thread_cache::ThreadCache {
        &self.threads
    }

    pub(crate) fn snapshot_shells(&self) -> &SnapshotShells {
        &self.snapshot_shells
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StoredEvent>> {
        self.tx.subscribe()
    }

    /// The newest event's sequence (0 with none). An error is an error: a
    /// 0 read as "nothing yet" would send clients a snapshot or a resume
    /// point that is not the log's.
    pub fn latest_sequence(&self) -> Result<u64, StoreError> {
        Ok(self.db.read(|c| c.prepare_cached("SELECT COALESCE(MAX(sequence), 0) FROM events")?.query_row([], |r| r.get::<_, i64>(0)))? as u64)
    }

    /// Which records the events in `(after, through]` changed, in order.
    pub fn events_after(&self, after: u64, through: u64) -> Result<Vec<EventHead>, StoreError> {
        Ok(self.db.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT sequence, aggregate_kind, aggregate_id, type FROM events
                 WHERE sequence > ?1 AND sequence <= ?2 ORDER BY sequence",
            )?;
            let rows = st.query_map(params![after as i64, through as i64], |r| {
                Ok(EventHead { sequence: r.get::<_, i64>(0)? as u64, aggregate_kind: r.get(1)?, aggregate_id: r.get(2)?, event_type: r.get(3)? })
            })?;
            rows.collect()
        })?)
    }

    /// Appends a project's event, whose JSON is `payload`, and applies its
    /// projection update atomically.
    pub fn append(
        &self,
        aggregate_kind: &str,
        aggregate_id: &str,
        event_type: &str,
        payload: &str,
        command_id: Option<&str>,
        project: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<()>,
    ) -> Result<Arc<StoredEvent>, StoreError> {
        let stored = self.db.with(|c| {
            let tx = c.savepoint()?;
            let sequence = insert_event(&tx, aggregate_kind, aggregate_id, event_type, payload, &domovoi_contracts::iso_now(), command_id)?;
            project(&tx)?;
            tx.commit()?;
            let stored = Arc::new(StoredEvent {
                sequence,
                aggregate_kind: aggregate_kind.into(),
                aggregate_id: aggregate_id.into(),
                event_type: event_type.into(),
                thread_event: None,
                grew_from: None,
            });
            self.publish(&stored);
            Ok(stored)
        })?;
        Ok(stored)
    }

    /// Once the event's transaction has committed (the open batch's, or the
    /// caller's own when there is none): applies a thread event to the thread
    /// cache, then tells subscribers. Call with the writer held, so both see
    /// events in commit order; a batch that rolls back publishes nothing. A
    /// batch's events all reach the cache before any reaches subscribers: a
    /// subscriber reading the thread for the first is not in the way of the next.
    pub(crate) fn publish(&self, stored: &Arc<StoredEvent>) {
        if stored.thread_event.is_some() {
            let (cache, event) = (self.threads.clone(), stored.clone());
            self.db.after_commit(move || cache.apply(&event));
        }
        let (tx, event) = (self.tx.clone(), stored.clone());
        self.db.after_commit_notify(move || {
            let _ = tx.send(event);
        });
    }

    // ---- projects ----

    pub fn list_projects(&self) -> Result<Vec<ProjectShell>, StoreError> {
        Ok(self.db.read(|c| {
            let mut st = c.prepare_cached("SELECT shell FROM projects WHERE deleted = 0 ORDER BY rowid")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.map(|s| decode_project(&s)).transpose()).collect()
        })?)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<ProjectShell>, StoreError> {
        Ok(self.db.read(|c| {
            c.prepare_cached("SELECT shell FROM projects WHERE id = ?1 AND deleted = 0")?
                .query_row(params![id], |r| r.get::<_, String>(0))
                .optional()
        })?
        .and_then(|s| decode_project(&s)))
    }

    pub fn create_project(&self, shell: ProjectShell, command_id: Option<&str>) -> Result<Arc<StoredEvent>, StoreError> {
        if self.list_projects()?.iter().any(|p| p.workspace_root == shell.workspace_root) {
            return Err(StoreError::Invalid(format!(
                "A project already exists for {}",
                shell.workspace_root
            )));
        }
        let json = serde_json::to_string(&shell).expect("plain data serializes");
        let id = shell.id.clone();
        self.append("project", &id, "project.created", &json, command_id, |tx| {
            tx.execute(
                "INSERT INTO projects (id, shell) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET shell = excluded.shell, deleted = 0",
                params![id, json],
            )
            .map(|_| ())
        })
    }

    /// Replaces a live project's shell (`project.meta-updated`).
    pub fn update_project(&self, shell: ProjectShell, command_id: Option<&str>) -> Result<Arc<StoredEvent>, StoreError> {
        if self.get_project(&shell.id)?.is_none() {
            return Err(StoreError::Invalid(format!("Unknown project {}", shell.id)));
        }
        if self.list_projects()?.iter().any(|p| p.id != shell.id && p.workspace_root == shell.workspace_root) {
            return Err(StoreError::Invalid(format!("A project already exists for {}", shell.workspace_root)));
        }
        let json = serde_json::to_string(&shell).expect("plain data serializes");
        let id = shell.id.clone();
        self.append("project", &id, "project.meta-updated", &json, command_id, |tx| {
            tx.execute("UPDATE projects SET shell = ?2 WHERE id = ?1", params![id, json]).map(|_| ())
        })
    }

    pub fn delete_project(&self, id: &str, command_id: Option<&str>) -> Result<Arc<StoredEvent>, StoreError> {
        if self.get_project(id)?.is_none() {
            return Err(StoreError::Invalid(format!("Unknown project {id}")));
        }
        self.append("project", id, "project.deleted", "null", command_id, |tx| {
            tx.execute("UPDATE projects SET deleted = 1 WHERE id = ?1", params![id]).map(|_| ())
        })
    }
}

/// A stored record, decoded. One this version cannot read (damaged, or
/// written by another) is left out with a warning: never read as `null`,
/// which a caller would send on or, editing it, write back over the record.
pub(crate) fn decode_record<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Option<T> {
    match serde_json::from_str(text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("skipping a stored {what} that does not decode: {e}");
            None
        }
    }
}

/// Inserts an event row inside `tx` and returns its sequence. The caller
/// commits, then broadcasts.
pub(crate) fn insert_event(
    tx: &rusqlite::Connection,
    aggregate_kind: &str,
    aggregate_id: &str,
    event_type: &str,
    payload: &str,
    occurred_at: &str,
    command_id: Option<&str>,
) -> rusqlite::Result<u64> {
    tx.prepare_cached(
        "INSERT INTO events (aggregate_kind, aggregate_id, type, payload, occurred_at, command_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
        .execute(params![aggregate_kind, aggregate_id, event_type, payload, occurred_at, command_id])?;
    Ok(tx.last_insert_rowid() as u64)
}

/// A stored project shell. A row this version cannot read (written by a
/// newer one, or damaged) is left out with a warning instead of failing
/// every request that lists projects.
fn decode_project(shell: &str) -> Option<ProjectShell> {
    serde_json::from_str(shell)
        .inspect_err(|e| tracing::warn!("skipping a project row that does not decode: {e}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(id: &str, root: &str) -> ProjectShell {
        ProjectShell {
            id: id.into(), title: "p".into(), has_custom_title: false, workspace_root: root.into(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None,
            auto_pull: false, favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None,
            archived_at: None, created_at: "t".into(), updated_at: "t".into(),
        }
    }

    #[tokio::test]
    async fn append_projects_and_broadcasts_in_sequence() {
        let store = Store::new(Db::open_in_memory().unwrap());
        let mut rx = store.subscribe();
        let e1 = store.create_project(shell("a", "/a"), Some("c1")).unwrap();
        let e2 = store.create_project(shell("b", "/b"), None).unwrap();
        assert_eq!((e1.sequence, e2.sequence), (1, 2));
        assert_eq!(rx.recv().await.unwrap().sequence, 1);
        assert_eq!(store.list_projects().unwrap().len(), 2);
        assert_eq!(store.events_after(1, 2).unwrap().len(), 1);
        assert!(matches!(store.create_project(shell("c", "/a"), None), Err(StoreError::Invalid(_))));
        store.delete_project("a", None).unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 1);
        assert_eq!(store.latest_sequence().unwrap(), 3);
    }

    #[test]
    fn a_project_row_that_does_not_decode_is_skipped() {
        let store = Store::new(Db::open_in_memory().unwrap());
        store.create_project(shell("a", "/a"), None).unwrap();
        store.db().with(|c| c.execute("INSERT INTO projects (id, shell) VALUES ('bad', '{\"id\": 7}')", [])).unwrap();
        let listed = store.list_projects().unwrap();
        assert_eq!(listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(store.get_project("bad").unwrap().is_none());
        assert!(store.get_project("a").unwrap().is_some());
    }
}
