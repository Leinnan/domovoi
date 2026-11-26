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
/// migration 16 found them.
pub const CURRENT: u32 = 1;

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
const STEPS: &[Step] = &[];

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
    let from = found(db)?;
    if from == current {
        return Ok(None);
    }
    let backup = backups.join(format!("domovoi-format-{from}-{}.sqlite", domovoi_contracts::now_millis()));
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

    fn home() -> (Db, PathBuf) {
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
    fn a_current_database_is_left_alone() {
        let (db, backups) = home();
        assert!(upgrade(&db, &backups).unwrap().is_none());
        assert!(!backups.exists(), "nothing was rewritten, so nothing was copied");
        assert_eq!(found(&db).unwrap(), 1);
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
