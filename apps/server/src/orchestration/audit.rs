//! Reads every stored record the way the server does and says which do not
//! decode (`domovoi-server doctor --records`).
//!
//! The server skips a row it cannot read, with a warning in the log: the
//! thread opens without that record. This walks the whole database instead of
//! waiting for each thread to be opened, so a change to how records are
//! written can be checked against a real home before and after it.

use super::stored;
use domovoi_model::typed::{AppThread, ProjectShell, RecordKind};
use serde::Serialize;
use std::collections::BTreeMap;

/// How many unreadable rows of one group are described.
const SAMPLES: usize = 3;

/// The rows of one table and kind.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub table: &'static str,
    /// The collection a thread's record is kept in, or an event's type.
    pub kind: String,
    pub rows: u64,
    pub bytes: u64,
    /// Rows that do not decode.
    pub unread: u64,
    /// The first few of them: which row, and why.
    pub samples: Vec<String>,
}

#[derive(Default)]
struct Groups(BTreeMap<(&'static str, String), Group>);

impl Groups {
    fn row(&mut self, table: &'static str, kind: &str, text: &str, which: impl FnOnce() -> String, read: Result<(), String>) {
        let group = self.0.entry((table, kind.to_owned())).or_insert_with(|| Group { table, kind: kind.to_owned(), ..Group::default() });
        group.rows += 1;
        group.bytes += text.len() as u64;
        if let Err(why) = read {
            group.unread += 1;
            if group.samples.len() < SAMPLES {
                group.samples.push(format!("{}: {why}", which()));
            }
        }
    }
}

fn outcome<T>(result: serde_json::Result<T>) -> Result<(), String> {
    result.map(drop).map_err(|e| e.to_string())
}

/// Every thread, thread record and project in the database, and with
/// `events` every logged thread event, grouped by table and kind.
pub fn records(c: &rusqlite::Connection, events: bool) -> rusqlite::Result<Vec<Group>> {
    let mut groups = Groups::default();

    let mut st = c.prepare("SELECT id, record FROM threads")?;
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let (id, text) = (row.get_ref(0)?.as_str()?, row.get_ref(1)?.as_str()?);
        groups.row("threads", "thread", text, || format!("thread {id}"), outcome(serde_json::from_str::<AppThread>(text)));
    }

    let mut st = c.prepare("SELECT thread_id, kind, entity_id, record FROM thread_entities")?;
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let (thread, kind, id, text) = (row.get_ref(0)?.as_str()?, row.get_ref(1)?.as_str()?, row.get_ref(2)?.as_str()?, row.get_ref(3)?.as_str()?);
        let read = match RecordKind::of_collection(kind) {
            Some(record_kind) => outcome(stored::record(record_kind, text)),
            None => Err("a collection this build does not know".to_owned()),
        };
        groups.row("thread_entities", kind, text, || format!("thread {thread}, {id}"), read);
    }

    let mut st = c.prepare("SELECT id, shell FROM projects WHERE deleted = 0")?;
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let (id, text) = (row.get_ref(0)?.as_str()?, row.get_ref(1)?.as_str()?);
        groups.row("projects", "project", text, || format!("project {id}"), outcome(serde_json::from_str::<ProjectShell>(text)));
    }

    if events {
        let mut st = c.prepare("SELECT sequence, type, payload FROM events WHERE aggregate_kind = 'thread'")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            let (sequence, event_type, text): (i64, _, _) = (row.get(0)?, row.get_ref(1)?.as_str()?, row.get_ref(2)?.as_str()?);
            groups.row("events", event_type, text, || format!("event {sequence}"), outcome(stored::event(event_type, text)));
        }
    }
    Ok(groups.0.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn unreadable_rows_are_counted_and_named_and_readable_ones_only_counted() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            c.execute_batch(
                r#"INSERT INTO threads (id, project_id, record, last_sequence) VALUES ('t1', 'p1', '{"not":"a thread"}', 1);
                   INSERT INTO thread_entities (thread_id, kind, entity_id, record, first_sequence) VALUES
                     ('t1', 'turnItems', 'i1', '{"type":"assistant_message"}', 1),
                     ('t1', 'turnItems', 'i2', 'not json', 2),
                     ('t1', 'widgets', 'w1', '{}', 3);
                   INSERT INTO projects (id, shell) VALUES ('p1', '{}');
                   INSERT INTO events (aggregate_kind, aggregate_id, type, payload, occurred_at) VALUES ('thread', 't1', 'run.created', '{}', 'now');"#,
            )
        })
        .unwrap();
        let without_events = db.read(|c| records(c, false)).unwrap();
        assert!(without_events.iter().all(|g| g.table != "events"));
        let groups = db.read(|c| records(c, true)).unwrap();
        let of = |table: &str, kind: &str| groups.iter().find(|g| g.table == table && g.kind == kind).unwrap_or_else(|| panic!("no group {table}/{kind} in {groups:?}"));
        let items = of("thread_entities", "turnItems");
        assert_eq!((items.rows, items.unread, items.samples.len()), (2, 2, 2));
        assert!(items.samples[0].starts_with("thread t1, i1: "), "{:?}", items.samples);
        assert_eq!(items.bytes, "{\"type\":\"assistant_message\"}".len() as u64 + "not json".len() as u64);
        assert_eq!(of("thread_entities", "widgets").samples, ["thread t1, w1: a collection this build does not know"]);
        assert_eq!(of("threads", "thread").unread, 1);
        assert_eq!(of("projects", "project").unread, 1);
        assert_eq!(of("events", "run.created").unread, 1);
    }
}
