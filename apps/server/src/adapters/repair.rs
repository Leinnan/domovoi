//! Repairs what an older server recorded wrong, once, at startup.
//!
//! Before background agents were understood, a Claude `Agent` call run in the
//! background was marked finished at once, with its launch receipt ("Async
//! agent launched successfully…") as its result, on the subagent row and in
//! its child thread. The agent's real report (`task_notification`) came later
//! and is in the provider log: it replaces the receipt here.

#![expect(clippy::disallowed_types, reason = "lines of the Claude provider log (the CLI's task_notification messages)")]

use super::claude::{is_background_launch, task_status};
use super::turn::spelled;
use crate::orchestration::events::{Kept, Origin};
use crate::store::{Store, decode_record};
use rusqlite::params;
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;
use domovoi_model::typed::{ConversationMessage, ExecutionNode, NodeStatus, Subagent, SubagentStatus, Timestamp, TurnItem, TurnItemStatus};

/// What an agent reported when it finished.
#[derive(Debug, Clone)]
struct Notice {
    status: &'static str,
    summary: String,
    at: String,
}

/// Said in place of the receipt when no report was logged (the log was
/// rotated away, or the agent never reported).
const NOT_RECORDED: &str = "This agent ran in the background; its result was not recorded.";

/// Replaces launch receipts shown as subagent results with the agents'
/// reports from `provider_logs`. Returns how many subagents it fixed.
pub fn background_receipts(store: &Store, provider_logs: &Path) -> usize {
    let rows: Vec<(String, String)> = store
        .db()
        .with(|c| {
            let mut st = c.prepare(
                "SELECT thread_id, record FROM thread_entities
                 WHERE kind = 'subagents' AND json_extract(record, '$.result') LIKE 'Async agent launched%'",
            )?;
            let rows = st.query_map(params![], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    let subs: Vec<(String, Subagent)> = rows
        .into_iter()
        .filter_map(|(thread, rec)| Some((thread, decode_record::<Subagent>(&rec, "subagent")?)))
        .filter(|(_, sub)| sub.result.as_deref().is_some_and(is_background_launch))
        .collect();
    if subs.is_empty() {
        return 0;
    }
    let wanted: Vec<String> = subs.iter().filter_map(|(_, s)| native_id(s)).collect();
    let notices = notices(provider_logs, &wanted);
    let mut fixed = 0;
    for (thread, sub) in subs {
        let notice = native_id(&sub).and_then(|id| notices.get(&id).cloned());
        match fix(store, &thread, sub, notice.as_ref()) {
            Ok(()) => fixed += 1,
            Err(e) => tracing::warn!(thread = %thread, "could not repair a background agent's result: {e}"),
        }
    }
    fixed
}

fn native_id(sub: &Subagent) -> Option<String> {
    sub.native_task_ref.as_ref()?.native_id.clone()
}

/// The last `task_notification` for each of `wanted` (Claude tool use ids)
/// in the Claude provider logs: lines of what the CLI said, as it said it.
fn notices(dir: &Path, wanted: &[String]) -> HashMap<String, Notice> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !name.starts_with("claude-") {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else { continue };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"task_notification\"") || !wanted.iter().any(|w| line.contains(w.as_str())) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let msg = &v["payload"];
            if msg["type"] != "system" || msg["subtype"] != "task_notification" {
                continue;
            }
            let (Some(tool), Some(summary)) = (msg["tool_use_id"].as_str(), msg["summary"].as_str()) else { continue };
            if summary.trim().is_empty() || !wanted.iter().any(|w| w == tool) {
                continue;
            }
            let at = v["t"].as_str().map_or_else(domovoi_contracts::iso_now, str::to_owned);
            out.insert(tool.to_owned(), Notice { status: task_status(msg["status"].as_str()), summary: summary.trim().to_owned(), at });
        }
    }
    out
}

fn fix(store: &Store, thread: &str, mut sub: Subagent, notice: Option<&Notice>) -> Result<(), crate::store::StoreError> {
    let origin = Origin { driver: Some(sub.driver.as_str().to_owned()), provider_instance_id: Some(sub.provider_instance_id.as_str().to_owned()), command_id: None };
    let result = notice.map(|n| n.summary.clone());
    // When the agent reported, and how it ended, where it did report.
    let reported = notice.map(|n| (n.status, Timestamp::from(n.at.as_str())));
    sub.result = result.clone();
    if let Some((status, at)) = &reported {
        sub.status = spelled(status).unwrap_or(SubagentStatus::Completed);
        sub.completed_at = Some(at.clone());
        sub.updated_at = at.clone();
    }
    let node_id = sub.id.clone();
    // The item that shows the agent: the node's item that carries a result.
    let item = store.records::<TurnItem>(thread)?.into_iter().find(|item| item.node_id() == Some(&node_id) && matches!(**item, TurnItem::Subagent(_)));
    let node = store.record::<ExecutionNode>(thread, node_id.as_str())?;
    let child = sub.child_thread_id.clone();
    store.append_event(thread, sub.updated(), &origin)?;
    if let Some(item) = item {
        let mut item = TurnItem::clone(&item);
        if let TurnItem::Subagent(shown) = &mut item {
            shown.result = result;
        }
        if let Some((status, at)) = &reported {
            item.set_status(spelled(status).unwrap_or(TurnItemStatus::Completed));
            item.set_completed_at(Some(at.clone()));
            item.set_updated_at(at.clone());
        }
        store.append_event(thread, item.updated(), &origin)?;
    }
    if let (Some((status, at)), Some(mut node)) = (&reported, node) {
        node.status = spelled(status).unwrap_or(NodeStatus::Completed);
        node.completed_at = Some(at.clone());
        store.append_event(thread, node.updated(), &origin)?;
    }

    // The child thread ends with the receipt as the agent's reply.
    let Some(child) = child else { return Ok(()) };
    let child = child.as_str();
    let text = notice.map_or(NOT_RECORDED, |n| n.summary.as_str());
    let messages = store.records::<ConversationMessage>(child)?;
    let items = store.records::<TurnItem>(child)?;
    for message in messages.iter().filter(|message| is_background_launch(&message.text)) {
        let mut message = ConversationMessage::clone(message);
        message.text = text.to_owned();
        if let Some((_, at)) = &reported {
            message.updated_at = at.clone();
        }
        let id = message.id.clone();
        store.append_event(child, message.updated(), &origin)?;
        for item in items.iter().filter(|item| item.message_id() == Some(&id)) {
            let mut item = TurnItem::clone(item);
            match &mut item {
                TurnItem::AssistantMessage(shown) => shown.text = text.to_owned(),
                TurnItem::UserMessage(shown) => shown.text = text.to_owned(),
                _ => {}
            }
            store.append_event(child, item.updated(), &origin)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::json;

    const RECEIPT: &str = "Async agent launched successfully. (This tool result is internal metadata.) agentId: a1";

    fn origin() -> Origin {
        Origin { driver: Some("claudeAgent".into()), provider_instance_id: Some("claudeAgent".into()), command_id: None }
    }

    /// A parent thread with one background agent recorded the old way.
    fn recorded(store: &Store, tool: &str) {
        use crate::test_records::{AT, append, assistant_item, message, node as node_record, thread, with};
        let o = origin();
        let child = format!("t1:subagent:{tool}");
        let node = format!("node:provider:claudeAgent:native-item:{tool}");
        append(store, "t1", "thread.created", thread("t1", "p1"), &o).unwrap();
        append(store, "t1", "node.updated", with(node_record("t1", &node, "completed"), json!({ "kind": "subagent" })), &o).unwrap();
        let subagent = json!({
            "id": node, "threadId": "t1", "parentNodeId": "root", "origin": "provider_native", "createdBy": "agent",
            "driver": "claudeAgent", "providerInstanceId": "claudeAgent", "childThreadId": child,
            "nativeTaskRef": { "driver": "claudeAgent", "nativeId": tool, "strength": "strong" }, "prompt": "Map the shell",
            "title": "Map the shell", "status": "completed", "result": RECEIPT, "completedAt": "2026-11-16T00:16:52.550Z", "updatedAt": AT,
        });
        append(store, "t1", "subagent.updated", subagent, &o).unwrap();
        let item = json!({
            "id": format!("turn-item:{tool}"), "threadId": "t1", "nodeId": node, "ordinal": 1, "updatedAt": AT, "type": "subagent",
            "subagentId": node, "origin": "provider_native", "driver": "claudeAgent", "providerInstanceId": "claudeAgent",
            "childThreadId": child, "prompt": "Map the shell", "status": "completed", "result": RECEIPT,
        });
        append(store, "t1", "turn-item.updated", item, &o).unwrap();
        append(store, &child, "thread.created", thread(&child, "p1"), &o).unwrap();
        append(store, &child, "message.updated", message(&child, "m-prompt", "user", "Map the shell", false), &o).unwrap();
        append(store, &child, "message.updated", message(&child, "m-result", "assistant", RECEIPT, false), &o).unwrap();
        append(store, &child, "turn-item.updated", assistant_item(&child, "i-result", "m-result", 1, RECEIPT, false), &o).unwrap();
    }

    #[test]
    fn a_receipt_shown_as_a_result_is_replaced_by_the_agents_report() {
        let store = Store::new(Db::open_in_memory().unwrap());
        recorded(&store, "toolu_A");
        recorded(&store, "toolu_B");
        let dir = std::env::temp_dir().join(format!("domovoi-repair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // The provider's log, as the CLI wrote it.
        let notice = json!({ "t": "2026-11-16T00:19:49.730Z", "direction": "incoming", "payload": {
            "type": "system", "subtype": "task_notification", "task_id": "a1", "tool_use_id": "toolu_A", "status": "completed",
            "summary": "## Findings\nThe shell drives the core through `Core.swift`." } });
        let noise = json!({ "t": "2026-11-16T00:19:00Z", "payload": { "type": "system", "subtype": "task_progress", "tool_use_id": "toolu_A" } });
        std::fs::write(dir.join("claude-s1.ndjson"), format!("{noise}\n{notice}\n")).unwrap();

        assert_eq!(background_receipts(&store, &dir), 2);
        const REPORT: &str = "## Findings\nThe shell drives the core through `Core.swift`.";
        let parent = store.load_thread("t1").unwrap().unwrap();
        let a = parent.subagents.get("node:provider:claudeAgent:native-item:toolu_A").unwrap();
        assert_eq!(a.result.as_deref(), Some(REPORT));
        assert_eq!((a.status.as_str(), a.completed_at.as_ref().map(Timestamp::as_str)), ("completed", Some("2026-11-16T00:19:49.730Z")));
        let TurnItem::Subagent(shown) = parent.turn_items.get("turn-item:toolu_A").unwrap() else { panic!("a subagent item") };
        assert_eq!(shown.result.as_deref(), Some(REPORT));
        let node = parent.nodes.get("node:provider:claudeAgent:native-item:toolu_A").unwrap();
        assert_eq!(node.completed_at.as_ref().map(Timestamp::as_str), Some("2026-11-16T00:19:49.730Z"));
        let child = store.load_thread("t1:subagent:toolu_A").unwrap().unwrap();
        assert_eq!(child.messages.get("m-result").unwrap().text, REPORT);
        assert_eq!(child.messages.get("m-prompt").unwrap().text, "Map the shell", "the prompt is left alone");
        let TurnItem::AssistantMessage(reply) = child.turn_items.get("i-result").unwrap() else { panic!("an assistant message") };
        assert_eq!(reply.text, REPORT);

        // No report logged: the receipt goes all the same, the status stays.
        let b = parent.subagents.get("node:provider:claudeAgent:native-item:toolu_B").unwrap();
        assert!(b.result.is_none());
        assert_eq!(b.completed_at.as_ref().map(Timestamp::as_str), Some("2026-11-16T00:16:52.550Z"));
        let child = store.load_thread("t1:subagent:toolu_B").unwrap().unwrap();
        assert_eq!(child.messages.get("m-result").unwrap().text, NOT_RECORDED);

        // Done once: a second start finds nothing to do.
        assert_eq!(background_receipts(&store, &dir), 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
