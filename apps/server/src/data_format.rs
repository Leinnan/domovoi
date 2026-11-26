//! The format of what the database's rows hold, beside its SQL schema.
//!
//! The schema (`PRAGMA user_version`, `db.rs`) says which tables and columns
//! exist. This number says how the records in them are written: a change that
//! needs each row read and written again is a step here, run in Rust once the
//! database is open and before anything else reads it.
//!
//! Before the first step runs, the whole database is copied to
//! `userdata/backups` ([`Db::backup_to`]). Each step commits what it changed
//! and then its number, so a server stopped half-way starts again at the step
//! it was in: a step must leave rows it has already rewritten as they are.
//!
//! A database written in a newer format is refused ([`refuse_newer`]): this
//! build would skip the records it cannot decode, and write back less than
//! it found.

use crate::db::Db;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The format this build reads and writes. Format 1 is the records as SQL
/// migration 16 found them. Format 2 changes no record: it is the event log
/// cut down to what a reconnecting client replays, and the file compacted.
/// Format 3 is without the execution node of every turn item.
pub const CURRENT: u32 = 3;

const KEY: &str = "data_format";

type StepError = Box<dyn std::error::Error + Send + Sync>;

/// One change of format: what leaves the database in format `to`.
pub struct Step {
    pub to: u32,
    /// What it does, for the log.
    pub what: &'static str,
    pub run: fn(&Db) -> Result<(), StepError>,
}

/// Every step, in order: `STEPS[n].to == n + 2`.
const STEPS: &[Step] = &[
    Step { to: 2, what: "the event log trimmed to what a reconnecting client replays", run: trim_event_log },
    Step { to: 3, what: "the execution nodes of turn items deleted", run: delete_item_nodes },
];

/// Until format 2 every event ever recorded was kept, most of the database's
/// size, though only the newest are ever read (`Store::trim_event_log`). The
/// server now trims the log as it goes; this drops what had piled up and
/// gives the space back to the disk.
fn trim_event_log(db: &Db) -> Result<(), StepError> {
    let keep = crate::orchestration::events::EVENT_LOG_KEEPS as i64;
    db.with(|c| {
        c.execute("DELETE FROM events WHERE sequence <= (SELECT COALESCE(MAX(sequence), 0) FROM events) - ?1", rusqlite::params![keep])?;
        c.execute_batch("VACUUM")
    })?;
    Ok(())
}

/// The kinds of execution node a turn recorded for each of its items, until
/// format 3: one beside every command, tool call, reasoning block, reply,
/// approval and task list, which nothing ever read. A run's root node, a
/// subagent's node and a plan's are read and stay.
const ITEM_NODE_KINDS: &[&str] = &["tool_call", "reasoning", "assistant_message", "approval_request", "todo_list", "system"];

fn delete_item_nodes(db: &Db) -> Result<(), StepError> {
    let kinds = ITEM_NODE_KINDS.iter().map(|kind| format!("'{kind}'")).collect::<Vec<_>>().join(", ");
    db.with(|c| {
        c.execute(&format!("DELETE FROM thread_entities WHERE kind = 'nodes' AND json_extract(record, '$.kind') IN ({kinds})"), [])?;
        c.execute_batch("VACUUM")
    })?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "the database holds records in format {found} and this build of domovoi-server knows format {known}: it was written by a newer version. Start that version, or restore a backup made before the update"
    )]
    Newer { found: u32, known: u32 },
    #[error("the database's format is recorded as `{0}`, which is not a number")]
    Unreadable(String),
    #[error("the database could not be copied to {} before its records are rewritten: {source}", path.display())]
    Backup { path: PathBuf, source: StepError },
    #[error("rewriting the records for format {to} ({what}) failed: {source}. The copy made before it is {}", backup.display())]
    Step { to: u32, what: &'static str, backup: PathBuf, source: StepError },
}

/// Whether the database holds no event, thread or project yet.
fn never_recorded_in(db: &Db) -> Result<bool, Error> {
    Ok(db.read(|c| {
        c.query_row("SELECT NOT EXISTS (SELECT 1 FROM events) AND NOT EXISTS (SELECT 1 FROM threads) AND NOT EXISTS (SELECT 1 FROM projects)", [], |r| r.get(0))
    })?)
}

/// What an upgrade did.
#[derive(Debug)]
pub struct Upgraded {
    pub from: u32,
    pub to: u32,
    pub backup: PathBuf,
    pub backup_took: Duration,
    /// Each step's format, description and time.
    pub steps: Vec<(u32, &'static str, Duration)>,
}

impl std::fmt::Display for Upgraded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "records rewritten from format {} to {}; copy of the old database: {} ({} ms)", self.from, self.to, self.backup.display(), self.backup_took.as_millis())?;
        for (to, what, took) in &self.steps {
            write!(f, "; format {to} ({what}): {} ms", took.as_millis())?;
        }
        Ok(())
    }
}

/// The format the database's records are in.
pub fn found(db: &Db) -> Result<u32, Error> {
    match db.meta_get(KEY)? {
        None => Ok(1),
        Some(value) => value.parse().map_err(|_| Error::Unreadable(value)),
    }
}

/// Fails when a newer server wrote the records.
pub fn refuse_newer(db: &Db) -> Result<(), Error> {
    refuse_newer_than(db, CURRENT)
}

fn refuse_newer_than(db: &Db, known: u32) -> Result<(), Error> {
    let found = found(db)?;
    if found > known { Err(Error::Newer { found, known }) } else { Ok(()) }
}

/// Brings the records to [`CURRENT`], copying the database into `backups`
/// first. `None` when they are current already.
pub fn upgrade(db: &Db, backups: &Path) -> Result<Option<Upgraded>, Error> {
    upgrade_with(db, backups, STEPS, CURRENT)
}

fn upgrade_with(db: &Db, backups: &Path, steps: &[Step], current: u32) -> Result<Option<Upgraded>, Error> {
    refuse_newer_than(db, current)?;
    // A database nothing was recorded in holds no record in an older format:
    // it is in this build's from the start, with nothing to copy or rewrite.
    if db.meta_get(KEY)?.is_none() && never_recorded_in(db)? {
        db.meta_set(KEY, &current.to_string())?;
        return Ok(None);
    }
    let from = found(db)?;
    if from == current {
        return Ok(None);
    }
    let backup = backups.join(format!("domovoi-format-{from}-{}.sqlite", crate::time_fmt::now_millis()));
    let started = Instant::now();
    let copied = crate::state::create_private_dir(backups).map_err(StepError::from).and_then(|()| db.backup_to(&backup).map_err(StepError::from));
    if let Err(source) = copied {
        return Err(Error::Backup { path: backup, source });
    }
    let mut done = Upgraded { from, to: from, backup, backup_took: started.elapsed(), steps: Vec::new() };
    for step in steps.iter().filter(|step| step.to > from && step.to <= current) {
        let started = Instant::now();
        (step.run)(db).map_err(|source| Error::Step { to: step.to, what: step.what, backup: done.backup.clone(), source })?;
        db.meta_set(KEY, &step.to.to_string())?;
        done.to = step.to;
        done.steps.push((step.to, step.what, started.elapsed()));
    }
    Ok(Some(done))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home a server has recorded in, with no format written (format 1).
    fn home() -> (Db, PathBuf) {
        let (db, backups) = new_home();
        db.with(|c| c.execute("INSERT INTO projects (id, shell) VALUES ('p1', '{}')", [])).unwrap();
        (db, backups)
    }

    fn new_home() -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!("domovoi-data-format-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        (Db::open(&dir.join("t.sqlite")).unwrap(), dir.join("backups"))
    }

    fn marks(db: &Db, key: &str) -> Result<(), StepError> {
        let so_far = db.meta_get(key)?.unwrap_or_default();
        Ok(db.meta_set(key, &format!("{so_far}x"))?)
    }

    const TWO: &[Step] = &[
        Step { to: 2, what: "first", run: |db| marks(db, "first") },
        Step { to: 3, what: "second", run: |db| marks(db, "second") },
    ];

    #[test]
    fn a_new_database_starts_in_this_builds_format() {
        let (db, backups) = new_home();
        assert!(upgrade_with(&db, &backups, TWO, 3).unwrap().is_none());
        assert!(!backups.exists(), "nothing was rewritten, so nothing was copied");
        assert_eq!(found(&db).unwrap(), 3);
        assert_eq!(db.meta_get("first").unwrap(), None, "no step ran");
        // And one already current is left alone.
        db.with(|c| c.execute("INSERT INTO projects (id, shell) VALUES ('p1', '{}')", [])).unwrap();
        assert!(upgrade_with(&db, &backups, TWO, 3).unwrap().is_none());
        assert!(!backups.exists());
    }

    /// Format 2: the events that had piled up go, the newest stay, and no
    /// record in the projection tables is touched.
    #[test]
    fn the_event_log_is_trimmed_to_its_newest_events() {
        let (db, backups) = home();
        let keep = crate::orchestration::events::EVENT_LOG_KEEPS as i64;
        db.with(|c| {
            c.execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
                 INSERT INTO events (aggregate_kind, aggregate_id, type, payload, occurred_at) SELECT 'thread', 't1', 'run.updated', '{}', 'now' FROM n",
                rusqlite::params![keep + 500],
            )?;
            c.execute("INSERT INTO threads (id, project_id, record, last_sequence) VALUES ('t1', 'p1', '{}', 1)", [])
        })
        .unwrap();
        let done = upgrade(&db, &backups).unwrap().expect("the step ran");
        assert_eq!((done.from, done.to), (1, CURRENT));
        let (events, oldest, newest): (i64, i64, i64) = db.read(|c| c.query_row("SELECT COUNT(*), MIN(sequence), MAX(sequence) FROM events", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))).unwrap();
        assert_eq!((events, oldest, newest), (keep, 501, keep + 500));
        assert_eq!(db.read(|c| c.query_row("SELECT COUNT(*) FROM threads", [], |r| r.get::<_, i64>(0))).unwrap(), 1);
        // The copy made first still holds every event.
        let copy = Db::open(&done.backup).unwrap();
        assert_eq!(copy.read(|c| c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))).unwrap(), keep + 500);
    }

    /// Format 3: the node recorded beside every turn item goes; a run's
    /// root, a subagent's node, a plan's and every other record stay.
    #[test]
    fn the_nodes_of_turn_items_are_deleted_and_the_ones_that_are_read_stay() {
        let (db, backups) = home();
        db.with(|c| {
            for (id, kind) in [("n-root", "root_turn"), ("n-sub", "subagent"), ("n-plan", "plan"), ("n-tool", "tool_call"), ("n-think", "reasoning"), ("n-reply", "assistant_message"), ("n-ask", "approval_request"), ("n-todo", "todo_list"), ("n-note", "system")] {
                c.execute(
                    "INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence) VALUES ('t1', 'nodes', ?1, ?2, 1)",
                    rusqlite::params![id, format!(r#"{{"id":"{id}","kind":"{kind}"}}"#)],
                )?;
            }
            // A turn item whose type is one of those kinds is no node.
            c.execute("INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence) VALUES ('t1', 'turnItems', 'i1', '{\"id\":\"i1\",\"kind\":\"tool_call\"}', 1)", [])
        })
        .unwrap();
        let done = upgrade(&db, &backups).unwrap().expect("the steps ran");
        assert_eq!(done.to, 3);
        let left: Vec<String> = db.read(|c| c.prepare("SELECT entity_id FROM thread_entities ORDER BY entity_id")?.query_map([], |r| r.get(0))?.collect()).unwrap();
        assert_eq!(left, ["i1", "n-plan", "n-root", "n-sub"]);
    }

    #[test]
    fn steps_run_once_in_order_after_a_copy_is_made() {
        let (db, backups) = home();
        db.meta_set("kept", "as it was").unwrap();
        let done = upgrade_with(&db, &backups, TWO, 3).unwrap().expect("two steps ran");
        assert_eq!((done.from, done.to), (1, 3));
        assert_eq!(done.steps.iter().map(|(to, what, _)| (*to, *what)).collect::<Vec<_>>(), [(2, "first"), (3, "second")]);
        assert_eq!(found(&db).unwrap(), 3);
        // The copy is the database as it was: readable, and without the steps' marks.
        let copy = Db::open(&done.backup).unwrap();
        assert_eq!(copy.meta_get("kept").unwrap().as_deref(), Some("as it was"));
        assert_eq!(copy.meta_get("first").unwrap(), None);
        // A second start finds nothing to do.
        assert!(upgrade_with(&db, &backups, TWO, 3).unwrap().is_none());
        assert_eq!(db.meta_get("first").unwrap().as_deref(), Some("x"));
        assert_eq!(db.meta_get("second").unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn a_failed_step_is_where_the_next_start_resumes() {
        const FAILS: &[Step] = &[
            Step { to: 2, what: "first", run: |db| marks(db, "first") },
            Step { to: 3, what: "second", run: |_| Err("no".into()) },
        ];
        let (db, backups) = home();
        let error = upgrade_with(&db, &backups, FAILS, 3).unwrap_err();
        assert!(matches!(error, Error::Step { to: 3, .. }), "{error}");
        assert!(error.to_string().contains("domovoi-format-1-"), "the message names the copy: {error}");
        assert_eq!(found(&db).unwrap(), 2, "the step that worked is recorded");
        let done = upgrade_with(&db, &backups, TWO, 3).unwrap().expect("the second step ran");
        assert_eq!(done.steps.len(), 1);
        assert_eq!(db.meta_get("first").unwrap().as_deref(), Some("x"), "the first step did not run twice");
    }

    #[test]
    fn records_from_a_newer_server_are_refused() {
        let (db, backups) = home();
        db.meta_set(KEY, "7").unwrap();
        assert!(matches!(refuse_newer(&db), Err(Error::Newer { found: 7, known: CURRENT })));
        let error = upgrade(&db, &backups).unwrap_err();
        assert!(error.to_string().contains("newer version"), "{error}");
        assert!(!backups.exists());
    }
}
