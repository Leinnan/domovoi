//! SQLite storage: one writer connection behind a mutex (writes are small and
//! serialized like t3code's orchestrator), and a pool of read-only connections
//! so a long read never holds up a write (WAL lets them run side by side).
//!
//! `Db::batch` runs a whole command in one transaction: every write inside it
//! (each its own savepoint) commits together or not at all, and the work
//! queued with `after_commit` (cache updates, broadcasts, effects) runs only
//! once the commit succeeds.

use parking_lot::ReentrantMutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Work deferred until the open batch commits.
type Deferred = Box<dyn FnOnce() + Send>;

/// Which round of a commit's deferred work a closure runs in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum When {
    Committed,
    Notify,
}

/// The writer connection. Re-entrant so the writes inside a batch, made by
/// code that does not know it runs in one, take the lock the batch holds.
struct Writer {
    conn: RefCell<Connection>,
    /// `Some` while a batch is open on the thread holding the lock.
    pending: RefCell<Option<Vec<(When, Deferred)>>>,
}

#[derive(Clone)]
pub struct Db {
    writer: Arc<ReentrantMutex<Writer>>,
    /// `None` for an in-memory database, whose readers use the writer.
    readers: Option<Arc<Readers>>,
}

/// Idle read-only connections to the database file.
struct Readers {
    path: std::path::PathBuf,
    idle: Mutex<Vec<Connection>>,
}

/// How many idle readers are kept; more are opened while reads overlap.
const IDLE_READERS: usize = 4;

/// Applied to every connection: wait for a lock instead of failing at once
/// (`domovoi-server pair` writes the same file from another process).
const CONNECTION_PRAGMAS: &str = "PRAGMA busy_timeout=5000;";

const MIGRATIONS: &[&str] = &[
    // 1: metadata, auth sessions, application event log, project projection.
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE auth_sessions (
        token_hash TEXT PRIMARY KEY, subject TEXT NOT NULL, scopes TEXT NOT NULL,
        permissions TEXT NOT NULL, created_at TEXT NOT NULL, expires_at TEXT NOT NULL,
        expires_at_ms INTEGER NOT NULL);
     CREATE TABLE events (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT, aggregate_kind TEXT NOT NULL,
        aggregate_id TEXT NOT NULL, type TEXT NOT NULL, payload TEXT NOT NULL,
        occurred_at TEXT NOT NULL, command_id TEXT);
     CREATE TABLE projects (id TEXT PRIMARY KEY, shell TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0);",
    // 2: orchestration v2 thread projection. Every entity is stored as its latest
    // full record (events carry whole records), ordered by first appearance.
    "CREATE TABLE threads (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL, record TEXT NOT NULL,
        last_sequence INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0);
     CREATE TABLE thread_entities (
        thread_id TEXT NOT NULL, kind TEXT NOT NULL, entity_id TEXT NOT NULL,
        record TEXT NOT NULL, first_sequence INTEGER NOT NULL,
        PRIMARY KEY (thread_id, kind, entity_id));
     CREATE INDEX events_aggregate ON events (aggregate_kind, aggregate_id, sequence);
     CREATE TABLE command_receipts (command_id TEXT PRIMARY KEY, result TEXT NOT NULL);",
    // 3: pairing grants live in the database so `domovoi-server pair` can mint a code
    // for a running server. Only hashes are stored; each code works once.
    "CREATE TABLE pairing_grants (
        code_hash TEXT PRIMARY KEY, scopes TEXT NOT NULL, permissions TEXT NOT NULL,
        created_at TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, consumed_at TEXT);",
    // 4: scheduled tasks (t3code `scheduled_tasks`, without webhook delivery logs)
    // and the transcripts each agent-session import came from.
    "CREATE TABLE scheduled_tasks (
        task_id TEXT PRIMARY KEY, title TEXT NOT NULL, prompt TEXT NOT NULL,
        enabled INTEGER NOT NULL, schedule_json TEXT NOT NULL, project_id TEXT NOT NULL,
        thread_id TEXT, thread_mode TEXT NOT NULL, workspace_strategy_json TEXT NOT NULL,
        model_selection_json TEXT NOT NULL, runtime_mode TEXT NOT NULL, interaction_mode TEXT NOT NULL,
        created_by TEXT NOT NULL, creation_source TEXT NOT NULL, created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL, next_run_at TEXT, last_run_at TEXT,
        last_run_status TEXT NOT NULL DEFAULT 'never', last_run_error TEXT,
        run_count INTEGER NOT NULL DEFAULT 0, webhook_token TEXT, webhook_secret TEXT);
     CREATE TABLE agent_session_imports (
        thread_id TEXT PRIMARY KEY, cwd TEXT NOT NULL, sources TEXT NOT NULL);",
    // 5: bearer sessions for native clients, sessions bound to an iroh peer,
    // and what each client said about itself when pairing.
    "ALTER TABLE auth_sessions ADD COLUMN method TEXT NOT NULL DEFAULT 'browser-session-cookie';
     ALTER TABLE auth_sessions ADD COLUMN bound_endpoint TEXT;
     ALTER TABLE auth_sessions ADD COLUMN client TEXT NOT NULL DEFAULT '{}';",
    // 6: APNs device tokens, each tied to the session that registered it.
    "CREATE TABLE push_devices (
        token TEXT PRIMARY KEY, session_hash TEXT NOT NULL, environment TEXT NOT NULL, created_at TEXT NOT NULL);",
    // 7: pairing codes that only work on this machine (the one printed at
    // start), and indexes for the lookups every iroh request and push makes.
    "ALTER TABLE pairing_grants ADD COLUMN local_only INTEGER NOT NULL DEFAULT 0;
     CREATE INDEX auth_sessions_bound ON auth_sessions (bound_endpoint);
     CREATE INDEX push_devices_session ON push_devices (session_hash);",
    // 8: one phone paired twice registers the same APNs token for two
    // sessions; each owns its own row.
    "CREATE TABLE push_devices_v2 (
        token TEXT NOT NULL, session_hash TEXT NOT NULL, environment TEXT NOT NULL, created_at TEXT NOT NULL,
        PRIMARY KEY (token, session_hash));
     INSERT INTO push_devices_v2 SELECT token, session_hash, environment, created_at FROM push_devices;
     DROP TABLE push_devices;
     ALTER TABLE push_devices_v2 RENAME TO push_devices;
     CREATE INDEX push_devices_session ON push_devices (session_hash);",
    // 9: project folders the automatic session import has seen: 'present'
    // while a project has that root, 'removed' once it is gone, which keeps
    // the import from bringing back a project you removed.
    "CREATE TABLE agent_session_roots (
        root TEXT PRIMARY KEY, state TEXT NOT NULL, updated_at TEXT NOT NULL);",
    // 10: effects that must outlive a restart (a rollback, a title or branch
    // name, a settle script), replayed at startup until done; receipts get a
    // time so old ones can be pruned.
    "CREATE TABLE effect_outbox (key TEXT PRIMARY KEY, effect TEXT NOT NULL, created_at TEXT NOT NULL);
     ALTER TABLE command_receipts ADD COLUMN created_at TEXT;",
];

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // NORMAL is safe with WAL: a crash of the server loses nothing; only a
        // power loss can undo the last commits, never corrupt the file.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(CONNECTION_PRAGMAS)?;
        let db = Db {
            writer: Arc::new(ReentrantMutex::new(Writer::new(conn))),
            readers: Some(Arc::new(Readers { path: path.to_owned(), idle: Mutex::new(Vec::new()) })),
        };
        db.migrate()?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let db = Db { writer: Arc::new(ReentrantMutex::new(Writer::new(Connection::open_in_memory()?))), readers: None };
        db.migrate()?;
        Ok(db)
    }

    /// Runs a read on a read-only connection: it sees every write committed
    /// before it started and never waits for the writer.
    ///
    /// Inside a batch it reads on the writer instead, so the batch sees its
    /// own uncommitted writes.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        if self.readers.is_none() || self.writer.is_owned_by_current_thread() {
            let guard = self.writer.lock();
            // Borrowed already only when called from inside a `with` closure:
            // then a reader sees the same committed state.
            if let Ok(conn) = guard.conn.try_borrow() {
                return f(&conn);
            }
        }
        let Some(readers) = &self.readers else {
            return Err(rusqlite::Error::InvalidParameterName("read from inside a write on an in-memory database".into()));
        };
        let idle = crate::util::locked(&readers.idle).pop();
        let conn = match idle {
            Some(c) => c,
            None => {
                let c = Connection::open_with_flags(
                    &readers.path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                c.execute_batch(CONNECTION_PRAGMAS)?;
                c
            }
        };
        let result = f(&conn);
        let mut idle = crate::util::locked(&readers.idle);
        if idle.len() < IDLE_READERS {
            idle.push(conn);
        }
        result
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        let guard = self.writer.lock();
        let mut conn = guard.conn.borrow_mut();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Runs `f` on the writer. A transaction `f` opens must be a savepoint
    /// (`Connection::savepoint`): inside a batch it nests in the batch's.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let guard = self.writer.lock();
        let mut conn = guard.conn.try_borrow_mut().map_err(|_| {
            rusqlite::Error::InvalidParameterName("the writer was used from inside a write on it".into())
        })?;
        f(&mut conn)
    }

    /// Runs `f` in one transaction: its writes commit together, or none do if
    /// it fails (or panics). Work queued with `after_commit` runs after the
    /// commit, in order, with the writer still held, so it sees commits in
    /// order; on failure it is dropped. A batch inside a batch joins it.
    ///
    /// `f` must not await or hand work to another thread that writes: the
    /// writer stays locked for the whole of it.
    pub fn batch<T, E: From<rusqlite::Error>>(&self, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let guard = self.writer.lock();
        if guard.pending.borrow().is_some() {
            return f();
        }
        guard.conn.borrow().execute_batch("BEGIN IMMEDIATE")?;
        *guard.pending.borrow_mut() = Some(Vec::new());
        let open = OpenBatch { writer: &guard, done: false };
        let result = f();
        open.finish(result.is_ok())?;
        result
    }

    /// Whether a batch is open on this thread: what it wrote is not committed yet.
    pub fn in_batch(&self) -> bool {
        self.writer.is_owned_by_current_thread() && self.writer.lock().pending.borrow().is_some()
    }

    /// Runs `f` once the batch open on this thread commits, or now if none is.
    pub fn after_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.defer(When::Committed, Box::new(f));
    }

    /// Like `after_commit`, after all of the batch's `after_commit` work: for
    /// telling others, once everything the batch changed in memory is in place.
    pub fn after_commit_notify(&self, f: impl FnOnce() + Send + 'static) {
        self.defer(When::Notify, Box::new(f));
    }

    fn defer(&self, when: When, f: Deferred) {
        if self.writer.is_owned_by_current_thread() {
            let guard = self.writer.lock();
            if let Some(pending) = guard.pending.borrow_mut().as_mut() {
                pending.push((when, f));
                return;
            }
        }
        f();
    }

    pub fn meta_get(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
                .optional()
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }
}

impl Writer {
    fn new(conn: Connection) -> Self {
        Writer { conn: RefCell::new(conn), pending: RefCell::new(None) }
    }
}

/// The batch open on the writer. Rolls back if dropped unfinished (a panic).
struct OpenBatch<'a> {
    writer: &'a Writer,
    done: bool,
}

impl OpenBatch<'_> {
    /// Commits (or rolls back) and runs the deferred work of a commit.
    fn finish(mut self, commit: bool) -> rusqlite::Result<()> {
        self.done = true;
        let deferred = self.writer.pending.borrow_mut().take().unwrap_or_default();
        let conn = self.writer.conn.borrow();
        if !commit {
            return conn.execute_batch("ROLLBACK");
        }
        if let Err(e) = conn.execute_batch("COMMIT") {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
        drop(conn);
        // In order, the notifications last.
        let (committed, notify): (Vec<_>, Vec<_>) = deferred.into_iter().partition(|(when, _)| *when == When::Committed);
        for (_, f) in committed.into_iter().chain(notify) {
            f();
        }
        Ok(())
    }
}

impl Drop for OpenBatch<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.writer.pending.borrow_mut().take();
            if let Ok(conn) = self.writer.conn.try_borrow() {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(db: &Db) -> Option<String> {
        db.read(|c| c.query_row("SELECT value FROM meta WHERE key = 'k'", [], |r| r.get(0)).optional()).unwrap()
    }

    #[test]
    fn a_failed_batch_leaves_nothing_and_runs_no_deferred_work() {
        let db = Db::open_in_memory().unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r: Result<(), rusqlite::Error> = db.batch(|| {
            db.meta_set("k", "v")?;
            let ran = ran.clone();
            db.after_commit(move || ran.store(true, std::sync::atomic::Ordering::SeqCst));
            assert_eq!(meta(&db).as_deref(), Some("v"), "the batch sees its own write");
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(r.is_err());
        assert_eq!(meta(&db), None);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn a_batch_commits_its_savepoints_together_and_then_runs_deferred_work_in_order() {
        let db = Db::open_in_memory().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        db.batch(|| {
            for i in 0..3 {
                db.with(|c| {
                    let sp = c.savepoint()?;
                    sp.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('k', ?1)", params![i.to_string()])?;
                    sp.commit()
                })?;
                let order = order.clone();
                db.after_commit(move || order.lock().unwrap().push(i));
            }
            assert!(order.lock().unwrap().is_empty(), "nothing runs before the commit");
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(meta(&db).as_deref(), Some("2"));
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn a_batchs_notifications_run_after_all_its_other_deferred_work() {
        let db = Db::open_in_memory().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let note = |what: &'static str| {
            let order = order.clone();
            move || order.lock().unwrap().push(what)
        };
        db.batch(|| {
            db.after_commit(note("state 1"));
            db.after_commit_notify(note("tell 1"));
            db.after_commit(note("state 2"));
            db.after_commit_notify(note("tell 2"));
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["state 1", "state 2", "tell 1", "tell 2"]);
        // With no batch open, each runs at once.
        db.after_commit_notify(note("now"));
        assert_eq!(order.lock().unwrap().last(), Some(&"now"));
    }

    #[test]
    fn a_panicking_batch_rolls_back_and_the_writer_stays_usable() {
        let db = Db::open_in_memory().unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), rusqlite::Error> = db.batch(|| {
                db.meta_set("k", "v").unwrap();
                panic!("handler bug");
            });
        }));
        assert!(caught.is_err());
        assert_eq!(meta(&db), None);
        db.meta_set("k", "after").unwrap();
        assert_eq!(meta(&db).as_deref(), Some("after"));
    }

    #[test]
    fn a_long_read_does_not_hold_up_a_write() {
        let dir = std::env::temp_dir().join(format!("domovoi-db-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.sqlite")).unwrap();
        db.meta_set("k", "before").unwrap();
        let (reading, read_started) = std::sync::mpsc::channel();
        let (written, write_done) = std::sync::mpsc::channel::<()>();
        let reader = db.clone();
        let seen = std::thread::spawn(move || {
            reader.read(|c| {
                let before: String = c.query_row("SELECT value FROM meta WHERE key = 'k'", [], |r| r.get(0))?;
                reading.send(()).unwrap();
                // Still inside the read while the write happens.
                write_done.recv_timeout(std::time::Duration::from_secs(5)).expect("the write went ahead");
                Ok(before)
            })
        });
        read_started.recv().unwrap();
        db.meta_set("k", "after").unwrap();
        written.send(()).unwrap();
        assert_eq!(seen.join().unwrap().unwrap(), "before", "a read sees the database as it started");
        assert_eq!(db.read(|c| c.query_row("SELECT value FROM meta WHERE key = 'k'", [], |r| r.get::<_, String>(0))).unwrap(), "after");
        let _ = std::fs::remove_dir_all(dir);
    }
}
