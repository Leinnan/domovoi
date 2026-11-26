//! Durable interactive terminal descriptors and optional inert screen text.

use crate::db::Db;
use rusqlite::params;

pub(super) struct Descriptor {
    pub(super) thread: String,
    pub(super) id: String,
    pub(super) cwd: String,
    pub(super) reported_cwd: Option<String>,
    pub(super) shell: String,
    pub(super) label: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) exit_code: Option<i32>,
    pub(super) updated_at: String,
    pub(super) screen: Option<String>,
}

pub(super) fn load(db: &Db) -> rusqlite::Result<Vec<Descriptor>> {
    db.read(|conn| {
        let mut rows = conn.prepare("SELECT thread_id, terminal_id, cwd, label, cols, rows, exit_code, updated_at, screen, reported_cwd, shell FROM terminal_descriptors")?;
        rows.query_map([], |row| {
            Ok(Descriptor {
                thread: row.get(0)?,
                id: row.get(1)?,
                cwd: row.get(2)?,
                label: row.get(3)?,
                cols: row.get(4)?,
                rows: row.get(5)?,
                exit_code: row.get(6)?,
                updated_at: row.get(7)?,
                screen: row.get(8)?,
                reported_cwd: row.get(9)?,
                shell: row.get(10)?,
            })
        })?.collect()
    })
}

pub(super) fn save(db: &Db, item: &super::Session) -> rusqlite::Result<()> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO terminal_descriptors (thread_id, terminal_id, cwd, label, cols, rows, exit_code, updated_at, reported_cwd, shell)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(thread_id, terminal_id) DO UPDATE SET cwd=excluded.cwd, label=excluded.label,
             cols=excluded.cols, rows=excluded.rows, exit_code=excluded.exit_code, updated_at=excluded.updated_at, reported_cwd=excluded.reported_cwd, shell=excluded.shell",
            params![item.thread_id, item.terminal_id, item.cwd, item.label, item.cols, item.rows, item.exit_code, item.updated_at, item.screen.callbacks().cwd, item.shell],
        )?;
        Ok(())
    })
}

pub(super) fn remove(db: &Db, thread: &str, id: &str) -> rusqlite::Result<()> {
    db.with(|conn| {
        conn.execute(
            "DELETE FROM terminal_descriptors WHERE thread_id=?1 AND terminal_id=?2",
            params![thread, id],
        )?;
        Ok(())
    })
}

pub(super) fn erase_screens(db: &Db) -> rusqlite::Result<()> {
    db.with(|conn| {
        conn.execute(
            "UPDATE terminal_descriptors SET screen=NULL WHERE screen IS NOT NULL",
            [],
        )?;
        Ok(())
    })
}

pub(super) fn capture(db: &Db, item: &super::Session) -> rusqlite::Result<()> {
    let mut screen = item.screen.screen().contents();
    if screen.len() > super::MAX_HISTORY_BYTES {
        let mut end = super::MAX_HISTORY_BYTES;
        while !screen.is_char_boundary(end) {
            end -= 1;
        }
        screen.truncate(end);
    }
    db.with(|conn| {
        conn.execute(
            "UPDATE terminal_descriptors SET screen=?3 WHERE thread_id=?1 AND terminal_id=?2",
            params![item.thread_id, item.terminal_id, screen],
        )?;
        Ok(())
    })
}

/// A verified provider session ID may point at one existing thread. If
/// several live records claim it, leave the link ambiguous.
#[cfg(unix)]
pub(super) fn thread_for_session(
    db: &Db,
    driver: &str,
    native: &str,
) -> rusqlite::Result<Option<String>> {
    db.read(|conn| {
        let mut query = conn.prepare(
            "SELECT DISTINCT entity.thread_id FROM thread_entities AS entity
             JOIN threads ON threads.id = entity.thread_id
             WHERE threads.deleted = 0 AND entity.kind = 'providerThreads'
               AND json_extract(entity.record, '$.driver') = ?1
               AND json_extract(entity.record, '$.nativeThreadRef.nativeId') = ?2
             LIMIT 2",
        )?;
        let mut rows = query.query(params![driver, native])?;
        let Some(first) = rows.next()?.map(|row| row.get(0)).transpose()? else {
            return Ok(None);
        };
        Ok(rows.next()?.is_none().then_some(first))
    })
}
