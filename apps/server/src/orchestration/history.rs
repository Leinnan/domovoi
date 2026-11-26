//! Indexed transcript pages and their snapshot watermark.

use super::{events, failure::Failure, stored};
use crate::store::Store;
use domovoi_model::typed::{
    RecordKind, RuntimeRequestStatus, SecretRequestStatus, StoredRecord, Subagent, SubagentStatus, Thread, ThreadHistory, ThreadSnapshot,
    TurnItem, TurnItemStatus,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::sync::Arc;

const PAGE_ITEMS: usize = 80;
const PAGE_BYTES: usize = 256 * 1024;
const COMMAND_PREVIEW_BYTES: usize = 2048;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInput {
    pub thread_id: String,
    pub before: Option<String>,
    pub around_entry: Option<String>,
}

fn failure(thread_id: &str, message: impl std::fmt::Display) -> Failure {
    Failure::ThreadProjection {
        thread_id: thread_id.to_owned(),
        message: message.to_string(),
    }
}

fn watermark(connection: &Connection) -> rusqlite::Result<u64> {
    connection.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| {
        Ok(row.get::<_, i64>(0)? as u64)
    })
}

fn page(
    connection: &Connection,
    thread_id: &str,
    before: Option<(i64, &str, bool)>,
    snapshot_sequence: u64,
    byte_limit: usize,
) -> rusqlite::Result<ThreadHistory> {
    let mut select = connection.prepare_cached(if before.is_some() {
        "SELECT first_sequence, entity_id, record FROM thread_entities
         WHERE thread_id = ?1 AND kind = 'turnItems' AND (first_sequence, entity_id) <= (?2, ?3)
           AND (?5 OR (first_sequence, entity_id) != (?2, ?3))
         ORDER BY first_sequence DESC, entity_id DESC LIMIT ?4"
    } else {
        "SELECT first_sequence, entity_id, record FROM thread_entities
         WHERE thread_id = ?1 AND kind = 'turnItems'
         ORDER BY first_sequence DESC, entity_id DESC LIMIT ?2"
    })?;
    let max_rows = (PAGE_ITEMS + 1) as i64;
    let mut rows = if let Some((seq, id, include)) = before {
        select.query(params![thread_id, seq, id, max_rows, include])?
    } else {
        select.query(params![thread_id, max_rows])?
    };
    let mut subagent_lookup =
        connection.prepare_cached("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = 'subagents' AND entity_id = ?2")?;
    let mut items = Vec::new();
    let mut subagents: Vec<Arc<Subagent>> = Vec::new();
    let mut cursor = None;
    let mut more = false;
    let mut bytes = 0;
    for row_index in 0..=PAGE_ITEMS {
        let Some(row) = rows.next()? else { break };
        if row_index == PAGE_ITEMS {
            more = true;
            break;
        }
        let text = row.get_ref(2)?.as_str()?;
        match stored::record(RecordKind::TurnItem, text) {
            Ok(StoredRecord::TurnItem(mut item)) => {
                command_preview(&mut item);
                Arc::make_mut(&mut item).strip_inline_images();
                let subagent = match &*item {
                    TurnItem::Subagent(item) if !subagents.iter().any(|agent| agent.id == item.subagent_id) => {
                        let record: Option<String> = subagent_lookup
                            .query_row(params![thread_id, item.subagent_id.as_str()], |row| row.get(0))
                            .optional()?;
                        record.and_then(|record| match stored::record(RecordKind::Subagent, &record) {
                            Ok(StoredRecord::Subagent(agent)) => Some(agent),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                let size = json_size(&item)? + subagent.as_ref().map(json_size).transpose()?.unwrap_or(0) + 2;
                if !items.is_empty() && bytes + size > byte_limit {
                    more = true;
                    break;
                }
                bytes += size;
                items.push(item);
                subagents.extend(subagent);
            }
            Ok(_) => unreachable!("reading turn items"),
            Err(error) => tracing::warn!(%error, "skipping invalid timeline record"),
        }
        cursor = Some(format!("{}:{}", row.get::<_, i64>(0)?, row.get_ref(1)?.as_str()?));
    }
    items.reverse();
    Ok(ThreadHistory {
        snapshot_sequence,
        history_before: cursor.filter(|_| more),
        items,
        subagents,
    })
}

fn json_size(value: &impl serde::Serialize) -> rusqlite::Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn command_preview(item: &mut Arc<TurnItem>) {
    let TurnItem::CommandExecution(command) = &**item else { return };
    if !matches!(
        command.status,
        TurnItemStatus::Completed | TurnItemStatus::Failed | TurnItemStatus::Cancelled
    ) || command.output.as_ref().is_none_or(|output| output.len() <= COMMAND_PREVIEW_BYTES)
    {
        return;
    }
    let TurnItem::CommandExecution(command) = Arc::make_mut(item) else {
        return;
    };
    if let Some(output) = &mut command.output {
        // Retain the first line, including any exit code, when it fits.
        let mut limit = COMMAND_PREVIEW_BYTES;
        while !output.is_char_boundary(limit) {
            limit -= 1;
        }
        output.truncate(limit);
        command.output_omitted = Some(true);
    }
}

fn pending_items(connection: &Connection, thread: &Thread) -> rusqlite::Result<Vec<Arc<TurnItem>>> {
    let mut stmt = connection.prepare_cached(
        "SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = 'turnItems'
         AND CASE WHEN json_valid(record) THEN
           json_extract(record, '$.type') IN ('approval_request', 'user_input_request')
           OR (json_extract(record, '$.type') = 'secret_request' AND json_extract(record, '$.secretStatus') = 'pending')
         ELSE 0 END",
    )?;
    let mut rows = stmt.query([thread.record.id.as_str()])?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next()? {
        let Ok(StoredRecord::TurnItem(item)) = stored::record(RecordKind::TurnItem, row.get_ref(0)?.as_str()?) else {
            continue;
        };
        let request = match &*item {
            TurnItem::ApprovalRequest(item) => Some(&item.request_id),
            TurnItem::UserInputRequest(item) => Some(&item.request_id),
            _ => None,
        };
        let awaiting_secret = matches!(&*item, TurnItem::SecretRequest(item) if item.secret_status == SecretRequestStatus::Pending);
        if awaiting_secret
            || request
                .and_then(|id| thread.runtime_requests.get(id))
                .is_some_and(|r| r.status == RuntimeRequestStatus::Pending)
        {
            pending.push(item);
        }
    }
    Ok(pending)
}

pub(super) fn snapshot(store: &Store, thread_id: &str) -> Result<ThreadSnapshot, Failure> {
    store
        .db()
        .read(|connection| {
            let tx = connection.unchecked_transaction()?;
            let at = watermark(&tx)?;
            let Some(thread) = events::load_thread_records(&tx, thread_id, false, false)? else {
                return Ok(None);
            };
            let mut projection = thread.projection();
            // Background work may refer to a finished agent while its provider
            // is still clearing the task. Keep those entries for the status check.
            if !projection
                .provider_threads
                .iter()
                .any(|provider| provider.pending_background_tasks.as_ref().is_some_and(|tasks| !tasks.is_empty()))
            {
                projection.subagents.retain(|agent| {
                    matches!(
                        agent.status,
                        SubagentStatus::Idle | SubagentStatus::Pending | SubagentStatus::Running | SubagentStatus::Waiting
                    )
                });
            }
            projection.turn_items = pending_items(&tx, &thread)?;
            let header_bytes = json_size(&projection)?;
            // Reserve room for the snapshot metadata, cursor and chunk envelope.
            let page = page(&tx, thread_id, None, at, PAGE_BYTES.saturating_sub(header_bytes + 512))?;
            projection
                .turn_items
                .retain(|awaiting| !page.items.iter().any(|item| item.id() == awaiting.id()));
            projection.turn_items.extend(page.items);
            for agent in page.subagents {
                if !projection.subagents.iter().any(|held| held.id == agent.id) {
                    projection.subagents.push(agent);
                }
            }
            Ok(Some(ThreadSnapshot {
                snapshot_sequence: at,
                projection,
                history_before: page.history_before,
            }))
        })
        .map_err(|e| failure(thread_id, e))?
        .ok_or_else(|| failure(thread_id, "Thread not found"))
}

fn entry_boundary(connection: &Connection, tid: &str, entry: &str) -> rusqlite::Result<Option<(i64, String)>> {
    let direct = connection
        .prepare_cached(
            "SELECT first_sequence, entity_id FROM thread_entities WHERE thread_id = ?1 AND kind = 'turnItems' AND entity_id = ?2",
        )?
        .query_row(params![tid, entry], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    if direct.is_some() {
        return Ok(direct);
    }
    connection
        .prepare_cached(
            "SELECT first_sequence, entity_id FROM thread_entities
         WHERE thread_id = ?1 AND kind = 'turnItems'
           AND CASE WHEN json_valid(record) THEN json_extract(record, '$.messageId') = ?2 ELSE 0 END
         ORDER BY first_sequence DESC, entity_id DESC LIMIT 1",
        )?
        .query_row(params![tid, entry], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
}

fn older(store: &Store, input: &HistoryInput) -> Result<ThreadHistory, Failure> {
    let tid = &input.thread_id;
    let before = match (input.before.as_deref(), input.around_entry.as_deref()) {
        (Some(cursor), None) => Some(
            cursor
                .split_once(':')
                .and_then(|(sequence, id)| Some((sequence.parse::<i64>().ok()?, id)))
                .filter(|(sequence, id)| *sequence >= 0 && !id.is_empty())
                .map(|(sequence, id)| (sequence, id, false))
                .ok_or_else(|| failure(tid, "Invalid history cursor"))?,
        ),
        (None, Some(entry)) if !entry.is_empty() => None,
        _ => return Err(failure(tid, "Specify a history cursor or an entry")),
    };
    store
        .db()
        .read(|connection| {
            let tx = connection.unchecked_transaction()?;
            let at = watermark(&tx)?;
            let exists = tx
                .query_row("SELECT 1 FROM threads WHERE id = ?1 AND deleted = 0", [tid], |_| Ok(()))
                .optional()?;
            if exists.is_none() {
                return Ok(None);
            }
            if let Some(entry) = input.around_entry.as_deref() {
                let Some((sequence, item_id)) = entry_boundary(&tx, tid, entry)? else {
                    return Ok(None);
                };
                return page(&tx, tid, Some((sequence, &item_id, true)), at, PAGE_BYTES).map(Some);
            }
            page(&tx, tid, before, at, PAGE_BYTES).map(Some)
        })
        .map_err(|e| failure(tid, e))?
        .ok_or_else(|| failure(tid, "Thread or transcript entry not found"))
}

pub async fn read_older(store: Store, input: HistoryInput) -> Result<ThreadHistory, Failure> {
    let tid = input.thread_id.clone();
    let permit = super::stream::SYNC_READS.acquire().await.map_err(|error| failure(&tid, error))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        older(&store, &input)
    })
    .await
    .map_err(|error| failure(&tid, error))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::events::Origin;
    use crate::test_records::{append, thread};
    use serde_json::json;

    fn seed(store: &Store, count: usize, output: &str) {
        let origin = Origin::default();
        append(store, "t1", "thread.created", thread("t1", "p"), &origin).unwrap();
        for ordinal in 0..count {
            append(
                store,
                "t1",
                "turn-item.updated",
                json!({
                    "type": "command_execution", "threadId": "t1", "id": format!("i{ordinal}"),
                    "ordinal": ordinal, "input": "run-tests", "output": output, "status": "completed",
                    "updatedAt": "2026-01-01T00:00:00.000Z",
                }),
                &origin,
            )
            .unwrap();
        }
    }

    #[test]
    fn window_pages_keep_history_and_detail_without_transferring_full_output() {
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let output = "日本語の出力\n".repeat(500);
        seed(&store, 101, &output);

        let recent = snapshot(&store, "t1").unwrap();
        assert_eq!(recent.projection.turn_items.len(), PAGE_ITEMS);
        assert_eq!(recent.projection.turn_items.first().unwrap().id().as_str(), "i21");
        assert!(serde_json::to_vec(&recent).unwrap().len() < PAGE_BYTES);
        let TurnItem::CommandExecution(preview) = &**recent.projection.turn_items.last().unwrap() else {
            panic!("command")
        };
        assert_eq!(preview.output_omitted, Some(true));
        assert!(output.starts_with(preview.output.as_deref().unwrap()));
        let original = store.record::<TurnItem>("t1", "i100").unwrap().unwrap();
        let TurnItem::CommandExecution(original) = original else {
            panic!("command")
        };
        assert_eq!(original.output.as_deref(), Some(output.as_str()));
        assert_eq!(original.output_omitted, None);

        let cursor = recent.history_before.unwrap();
        let earlier = older(
            &store,
            &HistoryInput {
                thread_id: "t1".into(),
                before: Some(cursor),
                around_entry: None,
            },
        )
        .unwrap();
        assert_eq!(earlier.snapshot_sequence, recent.snapshot_sequence);
        assert_eq!(earlier.items.len(), 21);
        assert_eq!(earlier.items.last().unwrap().id().as_str(), "i20");
        assert!(earlier.history_before.is_none());

        let around = older(
            &store,
            &HistoryInput {
                thread_id: "t1".into(),
                before: None,
                around_entry: Some("i25".into()),
            },
        )
        .unwrap();
        assert_eq!(around.items.last().unwrap().id().as_str(), "i25");
        assert_eq!(around.items.len(), 26);
    }

    fn compare_snapshots(store: &Store, thread_id: &str, label: &str) {
        use std::time::Instant;
        let mut results = [Vec::new(), Vec::new()];
        let mut bytes = [0, 0];
        for _ in 0..9 {
            for (variant, windowed) in [false, true].into_iter().enumerate() {
                let start = Instant::now();
                let projection = if windowed {
                    snapshot(store, thread_id)
                } else {
                    super::super::stream::snapshot(store, thread_id, false)
                }
                .unwrap();
                bytes[variant] = serde_json::to_vec(&projection).unwrap().len();
                results[variant].push(start.elapsed().as_micros());
            }
        }
        for (variant, kind) in ["full", "window"].into_iter().enumerate() {
            results[variant].sort_unstable();
            println!(
                "PERF thread.open.{kind}.{label}.build_encode {} us",
                results[variant][results[variant].len() / 2]
            );
            println!("PERF thread.open.{kind}.{label}.bytes {} bytes", bytes[variant]);
        }
    }

    #[test]
    #[ignore = "measurement; run with --ignored --nocapture"]
    fn perf_thread_window_by_history_length() {
        let output = (0..68)
            .map(|line| format!("{line:05} Test case completed with output from the scripted workspace command.\n"))
            .collect::<String>();
        for count in [793, 2_000, 10_000] {
            let store = Store::new(crate::db::Db::open_in_memory().unwrap());
            seed(&store, count, &output);
            compare_snapshots(&store, "t1", &count.to_string());
        }
    }

    #[test]
    #[ignore = "measurement on a scratch SQLite backup"]
    fn perf_thread_window_on_database_copy() {
        let (Ok(backup), Ok(thread_id)) = (std::env::var("DOMOVOI_PERF_DB_COPY"), std::env::var("DOMOVOI_PERF_THREAD_ID")) else {
            return;
        };
        let store = Store::new(crate::db::Db::open(std::path::Path::new(&backup)).unwrap());
        compare_snapshots(&store, &thread_id, "database_copy");
    }
}
