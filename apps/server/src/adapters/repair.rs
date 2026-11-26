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
use domovoi_model::typed::{
    Actor, AppThread, CommandExecutionItem, ConversationMessage, CreationSource, DynamicToolItem, ExecutionNode, FileChangeItem,
    ForkSource, InputIntent, MessageId, MessageRole, NodeId, NodeKind, NodeStatus, ParentRelationship, PlanArtifact, PlanId,
    PlanStatus, ProposedPlan, ProposedPlanItem, ProviderDriverKind, ProviderInstanceId, RunId, RunStatus, Subagent, SubagentItem,
    SubagentOrigin, SubagentStatus, ThreadChange, ThreadId, ThreadLineage, Timestamp, TurnItem, TurnItemId, TurnItemStatus,
    UserMessageItem, AssistantMessageItem,
};

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
            let at = v["t"].as_str().map_or_else(crate::time_fmt::iso_now, str::to_owned);
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

/// Backfills Antigravity plans that an older server recorded only as an empty
/// `file_change` (or a `file://` link to the agent's `brain` folder). Reads the
/// markdown from the instance's profile under `userdata` and records a
/// `ProposedPlan` so clients show the plan card and progress strip.
pub fn antigravity_plans(store: &Store, userdata: &Path) -> usize {
    let thread_ids: Vec<String> = store
        .db()
        .with(|c| {
            // A Windows path is `antigravity-acp\brain\…`, its backslashes
            // doubled in the JSON record.
            let mut st = c.prepare(
                r"SELECT DISTINCT thread_id FROM thread_entities
                 WHERE kind IN ('turnItems', 'messages')
                   AND (record LIKE '%antigravity-acp/brain/%' OR record LIKE '%antigravity-acp\\brain\\%')",
            )?;
            let rows = st.query_map(params![], |r| r.get(0))?;
            rows.collect()
        })
        .unwrap_or_default();
    let mut fixed = 0;
    for thread_id in thread_ids {
        match repair_antigravity_thread(store, userdata, &thread_id) {
            Ok(n) => fixed += n,
            Err(e) => tracing::warn!(thread = %thread_id, "could not repair Antigravity plans: {e}"),
        }
    }
    fixed
}

enum PlanRepairSource<'a> {
    FileChange(&'a FileChangeItem, String),
    AssistantLink {
        run_id: Option<RunId>,
        root_node_id: NodeId,
        provider_thread_id: Option<domovoi_model::typed::ProviderThreadId>,
        provider_turn_id: Option<domovoi_model::typed::ProviderTurnId>,
        ordinal: u64,
        updated_at: Timestamp,
        markdown: String,
    },
}

fn repair_antigravity_thread(store: &Store, userdata: &Path, thread_id: &str) -> Result<usize, crate::store::StoreError> {
    let Some(thread) = store.load_thread(thread_id)? else { return Ok(0) };
    if thread.record.deleted_at.is_some() {
        return Ok(0);
    }
    let runs_with_plan: std::collections::HashSet<Option<RunId>> = thread
        .plans
        .iter()
        .filter_map(|p| match p {
            PlanArtifact::ProposedPlan(plan) => Some(plan.run_id.clone()),
            _ => None,
        })
        .collect();
    let max_run_ordinal = thread
        .runs
        .iter()
        .filter(|r| r.status != RunStatus::RolledBack)
        .map(|r| r.ordinal)
        .max()
        .unwrap_or(0);
    let interaction_mode = thread.record.interaction_mode.as_str();

    let instance_for = |run_id: Option<&RunId>| -> String {
        run_id
            .and_then(|id| thread.runs.get(id))
            .map(|run| run.provider_instance_id.as_str().to_owned())
            .unwrap_or_else(|| thread.record.provider_instance_id.as_str().to_owned())
    };
    let run_rolled_back = |run_id: Option<&RunId>| -> bool {
        run_id.and_then(|id| thread.runs.get(id)).is_some_and(|r| r.status == RunStatus::RolledBack)
    };

    // Prefer the latest matching `FileChange` per run, else fall back to a `file://` brain link in an assistant reply.
    let mut candidates: HashMap<Option<RunId>, (String, PlanRepairSource<'_>)> = HashMap::new();
    for item in thread.turn_items.iter() {
        let TurnItem::FileChange(fc) = item else { continue };
        if runs_with_plan.contains(&fc.run_id) || run_rolled_back(fc.run_id.as_ref()) {
            continue;
        }
        let is_ag = fc.native_item_ref.as_ref().is_some_and(|r| r.driver.as_str() == crate::antigravity::DRIVER)
            || fc.id.as_str().contains(":antigravity:");
        if !is_ag {
            continue;
        }
        let Some(path) = fc.changes.as_ref().and_then(|c| c.first()).map(|c| c.path.as_str()) else { continue };
        let instance = instance_for(fc.run_id.as_ref());
        let profile = crate::antigravity::profile_in(userdata, &instance);
        let Some((canonical, markdown)) = crate::antigravity::plan::read_profile_brain_markdown(&profile.home, path) else { continue };
        if !crate::antigravity::plan::should_capture(&canonical, None, interaction_mode, false) {
            continue;
        }
        candidates.insert(fc.run_id.clone(), (instance, PlanRepairSource::FileChange(fc, markdown)));
    }

    for item in thread.turn_items.iter() {
        let TurnItem::AssistantMessage(msg) = item else { continue };
        if runs_with_plan.contains(&msg.run_id) || candidates.contains_key(&msg.run_id) || run_rolled_back(msg.run_id.as_ref()) {
            continue;
        }
        let instance = instance_for(msg.run_id.as_ref());
        let profile = crate::antigravity::profile_in(userdata, &instance);
        let text = thread.messages.get(&msg.message_id).map_or(msg.text.as_str(), |m| m.text.as_str());
        for linked in crate::antigravity::plan::markdown_file_links(text) {
            let Some((canonical, markdown)) = crate::antigravity::plan::read_profile_brain_markdown(&profile.home, &linked.to_string_lossy()) else {
                continue;
            };
            if !crate::antigravity::plan::should_capture(&canonical, None, interaction_mode, false) {
                continue;
            }
            let Some(root_node_id) = msg.run_id.as_ref().and_then(|id| thread.runs.get(id)).and_then(|r| r.root_node_id.clone()) else {
                continue;
            };
            let max_ord = thread
                .turn_items
                .iter()
                .filter(|other| other.run_id() == msg.run_id.as_ref())
                .map(TurnItem::ordinal)
                .max()
                .unwrap_or(msg.ordinal);
            candidates.insert(
                msg.run_id.clone(),
                (
                    instance,
                    PlanRepairSource::AssistantLink {
                        run_id: msg.run_id.clone(),
                        root_node_id,
                        provider_thread_id: msg.provider_thread_id.clone(),
                        provider_turn_id: msg.provider_turn_id.clone(),
                        ordinal: max_ord + 1,
                        updated_at: msg.updated_at.clone(),
                        markdown,
                    },
                ),
            );
            break;
        }
    }

    let mut fixed = 0;
    for (run_id, (instance, source)) in candidates {
        let is_latest_completed = run_id
            .as_ref()
            .and_then(|id| thread.runs.get(id))
            .is_some_and(|run| run.ordinal == max_run_ordinal && run.status == RunStatus::Completed);
        let plan_status = if is_latest_completed { PlanStatus::Active } else { PlanStatus::Completed };
        let origin = Origin {
            driver: Some(crate::antigravity::DRIVER.to_owned()),
            provider_instance_id: Some(instance),
            command_id: None,
        };
        store.db().batch(|| -> Result<(), crate::store::StoreError> {
            match source {
                PlanRepairSource::FileChange(fc, markdown) => {
                    let suffix = fc.id.as_str().strip_prefix("turn-item:").unwrap_or(fc.id.as_str());
                    let node_id = fc.node_id.clone().unwrap_or_else(|| NodeId::from(format!("node:{suffix}")));
                    if let Some(mut node) = store.record::<ExecutionNode>(thread_id, node_id.as_str())? {
                        node.kind = NodeKind::Plan;
                        node.status = NodeStatus::Completed;
                        store.append_event(thread_id, node.updated(), &origin)?;
                    }
                    let plan_id = PlanId::from(format!("plan:{suffix}"));
                    let artifact = PlanArtifact::ProposedPlan(ProposedPlan {
                        id: plan_id.clone(),
                        thread_id: fc.thread_id.clone(),
                        run_id: fc.run_id.clone(),
                        node_id: node_id.clone(),
                        status: plan_status,
                        detail_in_turn_item: None,
                        markdown: markdown.clone(),
                    });
                    store.append_event(thread_id, artifact.updated(), &origin)?;
                    let item = TurnItem::ProposedPlan(Box::new(ProposedPlanItem {
                        deferred_work: None,
                        tool_non_execution_kind: None,
                        tool_surface: None,
                        tool_icon: None,
                        tool_source: None,
                        id: fc.id.clone(),
                        thread_id: fc.thread_id.clone(),
                        run_id: fc.run_id.clone(),
                        node_id: Some(node_id),
                        provider_thread_id: fc.provider_thread_id.clone(),
                        provider_turn_id: fc.provider_turn_id.clone(),
                        native_item_ref: fc.native_item_ref.clone(),
                        parent_item_id: fc.parent_item_id.clone(),
                        ordinal: fc.ordinal,
                        status: TurnItemStatus::Completed,
                        title: fc.title.clone(),
                        started_at: fc.started_at.clone(),
                        completed_at: fc.completed_at.clone().or_else(|| Some(fc.updated_at.clone())),
                        updated_at: fc.updated_at.clone(),
                        plan_id,
                        markdown,
                        streaming: false,
                    }));
                    store.append_event(thread_id, item.updated(), &origin)?;
                }
                PlanRepairSource::AssistantLink {
                    run_id,
                    root_node_id,
                    provider_thread_id,
                    provider_turn_id,
                    ordinal,
                    updated_at,
                    markdown,
                } => {
                    let native_id = format!("antigravity-plan:{}", run_id.as_ref().map_or("0", RunId::as_str));
                    let enc = crate::orchestration::events::enc(&native_id);
                    let node_id = NodeId::from(format!("node:provider:antigravity:native-item:{enc}"));
                    let plan_id = PlanId::from(format!("plan:provider:antigravity:native-item:{enc}"));
                    let item_id = TurnItemId::from(format!("turn-item:provider:antigravity:native-item:{enc}"));
                    let native_ref = domovoi_model::typed::ProviderRef {
                        driver: ProviderDriverKind::from(crate::antigravity::DRIVER),
                        native_id: Some(native_id),
                        strength: domovoi_model::typed::NativeRefStrength::Strong,
                        fingerprint: None,
                        ordinal: None,
                    };
                    let node = ExecutionNode {
                        id: node_id.clone(),
                        thread_id: ThreadId::from(thread_id),
                        run_id: run_id.clone(),
                        parent_node_id: Some(root_node_id.clone()),
                        root_node_id,
                        kind: NodeKind::Plan,
                        status: NodeStatus::Completed,
                        counts_for_run: false,
                        provider_thread_id: provider_thread_id.clone(),
                        provider_turn_id: provider_turn_id.clone(),
                        native_item_ref: Some(native_ref.clone()),
                        runtime_request_id: None,
                        checkpoint_scope_id: None,
                        started_at: Some(updated_at.clone()),
                        completed_at: Some(updated_at.clone()),
                    };
                    store.append_event(thread_id, node.updated(), &origin)?;
                    let artifact = PlanArtifact::ProposedPlan(ProposedPlan {
                        id: plan_id.clone(),
                        thread_id: ThreadId::from(thread_id),
                        run_id: run_id.clone(),
                        node_id: node_id.clone(),
                        status: plan_status,
                        detail_in_turn_item: None,
                        markdown: markdown.clone(),
                    });
                    store.append_event(thread_id, artifact.updated(), &origin)?;
                    let item = TurnItem::ProposedPlan(Box::new(ProposedPlanItem {
                        deferred_work: None,
                        tool_non_execution_kind: None,
                        tool_surface: None,
                        tool_icon: None,
                        tool_source: None,
                        id: item_id,
                        thread_id: ThreadId::from(thread_id),
                        run_id,
                        node_id: Some(node_id),
                        provider_thread_id,
                        provider_turn_id,
                        native_item_ref: Some(native_ref),
                        parent_item_id: None,
                        ordinal,
                        status: TurnItemStatus::Completed,
                        title: None,
                        started_at: Some(updated_at.clone()),
                        completed_at: Some(updated_at.clone()),
                        updated_at,
                        plan_id,
                        markdown,
                        streaming: false,
                    }));
                    store.append_event(thread_id, item.updated(), &origin)?;
                }
            }
            Ok(())
        })?;
        fixed += 1;
    }
    Ok(fixed)
}

/// Backfills Antigravity `invoke_subagent` / `start_subagent` tool calls that
/// an older server recorded as raw `dynamic_tool` items. Converts each into a
/// `Subagent` entity + `TurnItem::Subagent` and populates its child thread from
/// the Antigravity session DB and child `transcript.jsonl` when available.
pub fn antigravity_subagents(store: &Store, userdata: &Path) -> usize {
    let thread_ids: Vec<String> = store
        .db()
        .with(|c| {
            let mut st = c.prepare(
                "SELECT DISTINCT thread_id FROM thread_entities
                 WHERE kind = 'turnItems'
                   AND (record LIKE '%invoke_subagent%' OR record LIKE '%start_subagent%')",
            )?;
            let rows = st.query_map(params![], |r| r.get(0))?;
            rows.collect()
        })
        .unwrap_or_default();
    let mut fixed = 0;
    for thread_id in thread_ids {
        match repair_antigravity_subagents_in_thread(store, userdata, &thread_id) {
            Ok(n) => fixed += n,
            Err(e) => tracing::warn!(thread = %thread_id, "could not repair Antigravity subagents: {e}"),
        }
    }
    fixed
}

fn is_repairable_subagent_tool(dt: &DynamicToolItem) -> bool {
    let is_ag = dt.native_item_ref.as_ref().is_some_and(|r| r.driver.as_str() == crate::antigravity::DRIVER)
        || dt.id.as_str().contains(":antigravity:");
    if !is_ag {
        return false;
    }
    let matches_name = |s: &str| {
        matches!(
            s,
            "invoke_subagent"
                | "start_subagent"
                | "Run invoke_subagent"
                | "Run invoke_subagent?"
                | "Running invoke_subagent"
                | "Run start_subagent"
                | "Run start_subagent?"
                | "Running start_subagent"
        )
    };
    dt.tool_name.as_deref().is_some_and(matches_name) || dt.title.as_deref().is_some_and(matches_name)
}

fn repair_antigravity_subagents_in_thread(store: &Store, userdata: &Path, thread_id: &str) -> Result<usize, crate::store::StoreError> {
    use crate::antigravity::child_work;
    use crate::orchestration::events::enc;

    let Some(thread) = store.load_thread(thread_id)? else { return Ok(0) };
    if thread.record.deleted_at.is_some() {
        return Ok(0);
    }

    let mut targets: Vec<&DynamicToolItem> = thread
        .turn_items
        .iter()
        .filter_map(|item| match item {
            TurnItem::DynamicTool(dt) if is_repairable_subagent_tool(dt) => Some(dt.as_ref()),
            _ => None,
        })
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    targets.sort_by_key(|dt| dt.ordinal);

    let mut fixed = 0;
    for (idx, dt) in targets.into_iter().enumerate() {
        let native_id = dt
            .native_item_ref
            .as_ref()
            .and_then(|r| r.native_id.clone())
            .or_else(|| dt.id.as_str().strip_prefix("turn-item:provider:antigravity:native-item:").map(str::to_owned))
            .unwrap_or_else(|| dt.id.as_str().to_owned());
        let instance = dt
            .run_id
            .as_ref()
            .and_then(|id| thread.runs.get(id))
            .map(|r| r.provider_instance_id.as_str().to_owned())
            .unwrap_or_else(|| thread.record.provider_instance_id.as_str().to_owned());
        let root_node_id = dt
            .run_id
            .as_ref()
            .and_then(|id| thread.runs.get(id))
            .and_then(|r| r.root_node_id.clone())
            .unwrap_or_else(|| NodeId::from(format!("node:run:{}", dt.run_id.as_ref().map_or("0", RunId::as_str))));
        let node_id = dt
            .node_id
            .clone()
            .unwrap_or_else(|| NodeId::from(format!("node:provider:antigravity:native-item:{}", enc(&native_id))));

        let input = dt.input.clone().unwrap_or(Value::Null);
        let (title, prompt) = child_work::launches(&input)
            .map(|l| child_work::describe(&l))
            .unwrap_or_else(|| (child_work::BATCH_TITLE.to_owned(), dt.title.clone().unwrap_or_else(|| child_work::BATCH_TITLE.to_owned())));
        let model = input["Model"]
            .as_str()
            .map(str::trim)
            .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("inherit"))
            .map(str::to_owned);

        let root_session = dt
            .provider_thread_id
            .as_ref()
            .or_else(|| dt.run_id.as_ref().and_then(|id| thread.runs.get(id)).and_then(|r| r.provider_thread_id.as_ref()))
            .and_then(|ptid| thread.provider_threads.get(ptid))
            .and_then(|pt| pt.native_thread_ref.as_ref())
            .and_then(|r| r.native_id.as_deref());

        let profile = crate::antigravity::profile_in(userdata, &instance);
        let snapshot = root_session.and_then(|sess| {
            let live = child_work::lookup_spawned_child(&profile.home, sess, &native_id)?;
            child_work::inspect_child(&profile.home, sess, &live.conversation_id, &live.log_uri, &prompt)
        });

        let (sub_status, item_status, node_status) = match snapshot.as_ref().map(|s| s.status) {
            Some(child_work::ChildState::Failed) => (SubagentStatus::Failed, TurnItemStatus::Failed, NodeStatus::Failed),
            Some(child_work::ChildState::Cancelled) => (SubagentStatus::Cancelled, TurnItemStatus::Cancelled, NodeStatus::Cancelled),
            _ if dt.status == TurnItemStatus::Failed => (SubagentStatus::Failed, TurnItemStatus::Failed, NodeStatus::Failed),
            _ => (SubagentStatus::Completed, TurnItemStatus::Completed, NodeStatus::Completed),
        };
        let result = snapshot.as_ref().and_then(|s| s.result.clone());

        let child = format!("{thread_id}:subagent:{}", enc(&native_id));
        let child_id = ThreadId::from(child.as_str());
        let ts = dt.updated_at.clone();
        let started_at = dt.started_at.clone().or_else(|| Some(ts.clone()));
        let completed_at = dt.completed_at.clone().or_else(|| Some(ts.clone()));
        let origin = Origin {
            driver: Some(crate::antigravity::DRIVER.to_owned()),
            provider_instance_id: Some(instance.clone()),
            command_id: None,
        };
        let child_exists = store.app_thread(&child)?.is_some();

        store.db().batch(|| -> Result<(), crate::store::StoreError> {
            if !child_exists {
                let child_thread = AppThread {
                    id: child_id.clone(),
                    title: super::subagents::child_title(&thread.record.title, Some(&title), &prompt, idx + 1),
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Provider,
                    pull_requests: Some(Vec::new()),
                    lineage: ThreadLineage {
                        parent_thread_id: Some(ThreadId::from(thread_id)),
                        relationship_to_parent: Some(ParentRelationship::Subagent),
                        root_thread_id: thread.record.lineage.root_thread_id.clone(),
                        side_chat: None,
                    },
                    forked_from: Some(ForkSource::Node { node_id: node_id.clone() }),
                    created_at: started_at.clone().unwrap_or_else(|| ts.clone()),
                    updated_at: ts.clone(),
                    active_provider_thread_id: None,
                    archived_at: None,
                    deleted_at: None,
                    title_regeneration: Some(None),
                    last_title_regeneration: Some(None),
                    pinned_at: Some(None),
                    pin_order_key: Some(None),
                    active_order_key: Some(None),
                    settled_override: Some(None),
                    settled_at: Some(None),
                    snoozed_until: Some(None),
                    snoozed_at: Some(None),
                    auto_settle_disabled_at: Some(None),
                    last_visited_at: Some(None),
                    opens: Some(None),
                    ..thread.record.clone()
                };
                store.append_event(&child, domovoi_model::typed::EventBody::Thread { change: ThreadChange::Created, thread: Box::new(child_thread) }, &origin)?;

                let child_root_id = NodeId::from(format!("node:subagent-root:{}", enc(&child)));
                let child_root = ExecutionNode {
                    id: child_root_id.clone(),
                    thread_id: child_id.clone(),
                    run_id: None,
                    parent_node_id: None,
                    root_node_id: child_root_id.clone(),
                    kind: NodeKind::RootTurn,
                    status: node_status.clone(),
                    counts_for_run: false,
                    provider_thread_id: None,
                    provider_turn_id: None,
                    native_item_ref: dt.native_item_ref.clone(),
                    runtime_request_id: None,
                    checkpoint_scope_id: None,
                    started_at: started_at.clone(),
                    completed_at: completed_at.clone(),
                };
                store.append_event(&child, child_root.updated(), &origin)?;

                let mut next_ord = 100u64;
                let append_child_msg = |next_ord: &mut u64, key: &str, role: MessageRole, body: &str| -> Result<(), crate::store::StoreError> {
                    let msg_id = MessageId::from(format!("message:subagent:{}:{}", enc(&child), enc(key)));
                    let item_id = TurnItemId::from(format!("turn-item:subagent:{}:{}", enc(&child), enc(key)));
                    let ord = *next_ord;
                    *next_ord += 1;
                    let from_user = role == MessageRole::User;
                    store.append_event(
                        &child,
                        ConversationMessage {
                            notification: None,
                            created_by: Actor::Agent,
                            creation_source: CreationSource::Provider,
                            scheduled_task_id: None,
                            sender_thread_id: None,
                            linked_sender: None,
                            idle_compaction: None,
                            workflow_script: None,
                            id: msg_id.clone(),
                            thread_id: child_id.clone(),
                            run_id: None,
                            node_id: Some(child_root_id.clone()),
                            role,
                            text: body.to_owned(),
                            context: None,
                            issue: None,
                            attachments: Vec::new(),
                            streaming: false,
                            created_at: ts.clone(),
                            updated_at: ts.clone(),
                            delegated_completion: None,
                        }
                        .updated(),
                        &origin,
                    )?;
                    let item = if from_user {
                        TurnItem::UserMessage(Box::new(UserMessageItem {
                            deferred_work: None,
                            tool_non_execution_kind: None,
                            tool_surface: None,
                            tool_icon: None,
                            tool_source: None,
                            id: item_id,
                            thread_id: child_id.clone(),
                            run_id: None,
                            node_id: Some(child_root_id.clone()),
                            provider_thread_id: None,
                            provider_turn_id: None,
                            native_item_ref: None,
                            parent_item_id: None,
                            ordinal: ord,
                            status: TurnItemStatus::Completed,
                            title: None,
                            started_at: started_at.clone(),
                            completed_at: started_at.clone(),
                            updated_at: ts.clone(),
                            created_by: Actor::Agent,
                            creation_source: CreationSource::Provider,
                            message_id: msg_id,
                            scheduled_task_id: None,
                            sender_thread_id: Some(ThreadId::from(thread_id)),
                            linked_sender: None,
                            idle_compaction: None,
                            workflow_script: None,
                            fresh_context: None,
                            input_intent: InputIntent::TurnStart,
                            text: body.to_owned(),
                            text_omitted: None,
                            context: None,
                            issue: None,
                            attachments: Vec::new(),
                            model_switch: None,
                        }))
                    } else {
                        TurnItem::AssistantMessage(Box::new(AssistantMessageItem {
                            deferred_work: None,
                            tool_non_execution_kind: None,
                            tool_surface: None,
                            tool_icon: None,
                            tool_source: None,
                            id: item_id,
                            thread_id: child_id.clone(),
                            run_id: None,
                            node_id: Some(child_root_id.clone()),
                            provider_thread_id: None,
                            provider_turn_id: None,
                            native_item_ref: None,
                            parent_item_id: None,
                            ordinal: ord,
                            status: TurnItemStatus::Completed,
                            title: None,
                            started_at: completed_at.clone(),
                            completed_at: completed_at.clone(),
                            updated_at: ts.clone(),
                            phase: None,
                            message_id: msg_id,
                            text: body.to_owned(),
                            text_omitted: None,
                            attachments: None,
                            streaming: false,
                        }))
                    };
                    store.append_event(&child, item.updated(), &origin)?;
                    Ok(())
                };

                append_child_msg(&mut next_ord, &format!("{native_id}:prompt"), MessageRole::User, &prompt)?;

                if let Some(snap) = &snapshot {
                    let mut open_tools: std::collections::VecDeque<(TurnItemId, u64, String, Value)> = Default::default();
                    let terminal_result = result.as_deref().map(str::trim);
                    for step in &snap.steps {
                        match step.kind.as_str() {
                            "PLANNER_RESPONSE" => {
                                for (ti, tc) in step.tool_calls.iter().enumerate() {
                                    let key = format!("{native_id}:s{}:t{ti}", step.index);
                                    let tid = TurnItemId::from(format!("turn-item:subagent:{}:{}", enc(&child), enc(&key)));
                                    let ord = next_ord;
                                    next_ord += 1;
                                    open_tools.push_back((tid, ord, tc.name.clone(), tc.input.clone()));
                                }
                                if let Some(text) = step.text.as_deref().map(str::trim).filter(|t| !t.is_empty())
                                    && terminal_result != Some(text)
                                {
                                    append_child_msg(&mut next_ord, &format!("{native_id}:s{}", step.index), MessageRole::Assistant, text)?;
                                }
                            }
                            "GENERIC" => {
                                if let Some((tid, ord, name, input)) = open_tools.pop_front() {
                                    let tstatus = if matches!(step.status.as_deref(), Some("ERROR" | "FAILED")) { TurnItemStatus::Failed } else { TurnItemStatus::Completed };
                                    let item = if matches!(name.as_str(), "Bash" | "run_command") {
                                        TurnItem::CommandExecution(Box::new(CommandExecutionItem {
                                            deferred_work: None,
                                            tool_non_execution_kind: None,
                                            tool_surface: None,
                                            tool_icon: None,
                                            tool_source: None,
                                            id: tid,
                                            thread_id: child_id.clone(),
                                            run_id: None,
                                            node_id: Some(child_root_id.clone()),
                                            provider_thread_id: None,
                                            provider_turn_id: None,
                                            native_item_ref: None,
                                            parent_item_id: None,
                                            ordinal: ord,
                                            status: tstatus,
                                            title: None,
                                            started_at: started_at.clone(),
                                            completed_at: completed_at.clone(),
                                            updated_at: ts.clone(),
                                            input: input["command"].as_str().or_else(|| input["CommandLine"].as_str()).unwrap_or_default().to_owned(),
                                            output: step.text.clone(),
                                            output_omitted: None,
                                            output_indicates_failure: None,
                                            exit_code: None,
                                            monitor: None,
                                            completion_wake: None,
                                        }))
                                    } else {
                                        TurnItem::DynamicTool(Box::new(DynamicToolItem {
                                            deferred_work: None,
                                            tool_non_execution_kind: None,
                                            tool_surface: None,
                                            tool_icon: None,
                                            tool_source: None,
                                            id: tid,
                                            thread_id: child_id.clone(),
                                            run_id: None,
                                            node_id: Some(child_root_id.clone()),
                                            provider_thread_id: None,
                                            provider_turn_id: None,
                                            native_item_ref: None,
                                            parent_item_id: None,
                                            ordinal: ord,
                                            status: tstatus,
                                            title: None,
                                            started_at: started_at.clone(),
                                            completed_at: completed_at.clone(),
                                            updated_at: ts.clone(),
                                            tool_name: Some(name),
                                            viewed_image_path: None,
                                            input: Some(input),
                                            output: step.text.clone().map(Value::String),
                                            output_omitted: None,
                                        }))
                                    };
                                    store.append_event(&child, item.updated(), &origin)?;
                                }
                            }
                            _ => {}
                        }
                    }
                    while let Some((tid, ord, name, input)) = open_tools.pop_front() {
                        let item = TurnItem::DynamicTool(Box::new(DynamicToolItem {
                            deferred_work: None,
                            tool_non_execution_kind: None,
                            tool_surface: None,
                            tool_icon: None,
                            tool_source: None,
                            id: tid,
                            thread_id: child_id.clone(),
                            run_id: None,
                            node_id: Some(child_root_id.clone()),
                            provider_thread_id: None,
                            provider_turn_id: None,
                            native_item_ref: None,
                            parent_item_id: None,
                            ordinal: ord,
                            status: item_status.clone(),
                            title: None,
                            started_at: started_at.clone(),
                            completed_at: completed_at.clone(),
                            updated_at: ts.clone(),
                            tool_name: Some(name),
                            viewed_image_path: None,
                            input: Some(input),
                            output: None,
                            output_omitted: None,
                        }));
                        store.append_event(&child, item.updated(), &origin)?;
                    }
                }
                if let Some(res) = result.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    append_child_msg(&mut next_ord, &format!("{native_id}:result"), MessageRole::Assistant, res)?;
                }
            }

            if let Some(mut node) = store.record::<ExecutionNode>(thread_id, node_id.as_str())? {
                node.kind = NodeKind::Subagent;
                node.status = node_status.clone();
                store.append_event(thread_id, node.updated(), &origin)?;
            } else {
                let node = ExecutionNode {
                    id: node_id.clone(),
                    thread_id: ThreadId::from(thread_id),
                    run_id: dt.run_id.clone(),
                    parent_node_id: Some(root_node_id.clone()),
                    root_node_id: root_node_id.clone(),
                    kind: NodeKind::Subagent,
                    status: node_status.clone(),
                    counts_for_run: false,
                    provider_thread_id: dt.provider_thread_id.clone(),
                    provider_turn_id: dt.provider_turn_id.clone(),
                    native_item_ref: dt.native_item_ref.clone(),
                    runtime_request_id: None,
                    checkpoint_scope_id: None,
                    started_at: started_at.clone(),
                    completed_at: completed_at.clone(),
                };
                store.append_event(thread_id, node.updated(), &origin)?;
            }

            let entity = Subagent {
                id: node_id.clone(),
                thread_id: ThreadId::from(thread_id),
                run_id: dt.run_id.clone(),
                parent_node_id: root_node_id,
                origin: SubagentOrigin::ProviderNative,
                created_by: Actor::Agent,
                driver: ProviderDriverKind::from(crate::antigravity::DRIVER),
                provider_instance_id: ProviderInstanceId::from(instance.as_str()),
                provider_thread_id: dt.provider_thread_id.clone(),
                child_thread_id: Some(child_id.clone()),
                remote_child: None,
                native_task_ref: dt.native_item_ref.clone(),
                prompt: prompt.clone(),
                title: Some(title.clone()),
                model,
                model_selection: None,
                completion_wake: None,
                completion_delivery: None,
                cancelled_run_id: None,
                status: sub_status,
                progress: None,
                workflow: None,
                result: result.clone(),
                started_at: started_at.clone(),
                completed_at: completed_at.clone(),
                updated_at: ts.clone(),
                prompt_omitted: None,
                result_omitted: None,
            };
            store.append_event(thread_id, entity.updated(), &origin)?;

            let item = TurnItem::Subagent(Box::new(SubagentItem {
                deferred_work: None,
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id: dt.id.clone(),
                thread_id: dt.thread_id.clone(),
                run_id: dt.run_id.clone(),
                node_id: Some(node_id.clone()),
                provider_thread_id: dt.provider_thread_id.clone(),
                provider_turn_id: dt.provider_turn_id.clone(),
                native_item_ref: dt.native_item_ref.clone(),
                parent_item_id: dt.parent_item_id.clone(),
                ordinal: dt.ordinal,
                status: item_status,
                title: Some(title),
                started_at,
                completed_at,
                updated_at: ts,
                subagent_id: node_id,
                origin: SubagentOrigin::ProviderNative,
                driver: ProviderDriverKind::from(crate::antigravity::DRIVER),
                provider_instance_id: ProviderInstanceId::from(instance.as_str()),
                child_thread_id: Some(child_id),
                remote_child: None,
                prompt,
                progress: None,
                result,
                prompt_omitted: None,
                result_omitted: None,
            }));
            store.append_event(thread_id, item.updated(), &origin)?;
            Ok(())
        })?;
        fixed += 1;
    }
    Ok(fixed)
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

    #[test]
    fn older_antigravity_brain_plan_files_are_backfilled_as_proposed_plans() {
        use crate::test_records::{AT, append, assistant_item, message, node as node_record, run, thread, with};

        let store = Store::new(Db::open_in_memory().unwrap());
        let userdata = std::env::temp_dir().join(format!("domovoi-repair-ag-plan-{}", uuid::Uuid::new_v4()));
        let profile = crate::antigravity::profile_in(&userdata, "antigravity");
        let brain = profile.acp.join("brain").join("sess-abc");
        std::fs::create_dir_all(&brain).unwrap();
        let plan_path = brain.join("claude_artifacts_plan.md");
        let full_plan = format!("# Full Plan\n\n{}\n\n## End", "Step detail line.\n".repeat(600));
        std::fs::write(&plan_path, &full_plan).unwrap();

        let o = Origin { driver: Some("antigravity".into()), provider_instance_id: Some("antigravity".into()), command_id: None };
        // Thread 1: latest completed run has an empty file_change pointing to brain plan -> becomes Active ProposedPlan.
        append(&store, "t-ag-1", "thread.created", with(thread("t-ag-1", "p1"), json!({ "providerInstanceId": "antigravity", "interactionMode": "plan" })), &o).unwrap();
        append(&store, "t-ag-1", "run.created", with(run("t-ag-1", "r1", "completed"), json!({ "ordinal": 1, "providerInstanceId": "antigravity", "rootNodeId": "root-1" })), &o).unwrap();
        let fc_node = "node:provider:antigravity:native-item:call_plan";
        let fc_item = "turn-item:provider:antigravity:native-item:call_plan";
        append(&store, "t-ag-1", "node.updated", with(node_record("t-ag-1", fc_node, "completed"), json!({ "runId": "r1", "kind": "tool_call" })), &o).unwrap();
        append(&store, "t-ag-1", "turn-item.updated", json!({
            "type": "file_change", "id": fc_item, "threadId": "t-ag-1", "runId": "r1", "nodeId": fc_node,
            "nativeItemRef": { "driver": "antigravity", "nativeId": "call_plan", "strength": "strong" },
            "ordinal": 1000072, "status": "completed", "updatedAt": AT, "fileName": "claude_artifacts_plan.md",
            "changes": [{ "operation": "add", "path": plan_path.to_string_lossy() }]
        }), &o).unwrap();

        // Thread 2: run 1 had a plan link in assistant reply, run 2 followed -> repaired as Completed ProposedPlan.
        let plan2 = brain.join("app_icon_plan.md");
        std::fs::write(&plan2, "# App Icon Plan\n\n1. Draw icon").unwrap();
        let link_text = format!("See [app_icon_plan.md]({}).", url::Url::from_file_path(&plan2).unwrap());
        append(&store, "t-ag-2", "thread.created", with(thread("t-ag-2", "p1"), json!({ "providerInstanceId": "antigravity", "interactionMode": "default" })), &o).unwrap();
        append(&store, "t-ag-2", "run.created", with(run("t-ag-2", "r1", "completed"), json!({ "ordinal": 1, "providerInstanceId": "antigravity", "rootNodeId": "root-2-1" })), &o).unwrap();
        append(&store, "t-ag-2", "run.created", with(run("t-ag-2", "r2", "completed"), json!({ "ordinal": 2, "providerInstanceId": "antigravity", "rootNodeId": "root-2-2" })), &o).unwrap();
        append(&store, "t-ag-2", "message.updated", with(message("t-ag-2", "m1", "assistant", &link_text, false), json!({ "runId": "r1" })), &o).unwrap();
        append(&store, "t-ag-2", "turn-item.updated", with(assistant_item("t-ag-2", "i-m1", "m1", 1000005, &link_text, false), json!({ "runId": "r1" })), &o).unwrap();

        assert_eq!(antigravity_plans(&store, &userdata), 2);

        let t1 = store.load_thread("t-ag-1").unwrap().unwrap();
        assert!(t1.shell().has_actionable_proposed_plan, "t1 has an active proposed plan");
        let TurnItem::ProposedPlan(item) = t1.turn_items.get(fc_item).expect("replaced in place") else { panic!("expected ProposedPlan") };
        assert_eq!(item.ordinal, 1000072);
        assert_eq!(item.markdown, full_plan.trim());
        assert_eq!(t1.nodes.get(fc_node).unwrap().kind, NodeKind::Plan);
        let PlanArtifact::ProposedPlan(plan) = t1.plans.get(&item.plan_id).unwrap() else { panic!("expected ProposedPlan artifact") };
        assert_eq!(plan.status, PlanStatus::Active);

        let t2 = store.load_thread("t-ag-2").unwrap().unwrap();
        assert!(!t2.shell().has_actionable_proposed_plan, "older run's plan is completed, not actionable");
        let PlanArtifact::ProposedPlan(plan2_rec) = t2.plans.iter().next().expect("repaired from link") else { panic!("expected ProposedPlan") };
        assert_eq!(plan2_rec.status, PlanStatus::Completed);
        assert_eq!(plan2_rec.markdown, "# App Icon Plan\n\n1. Draw icon");

        // Idempotent: a second startup repairs nothing.
        assert_eq!(antigravity_plans(&store, &userdata), 0);
        let _ = std::fs::remove_dir_all(&userdata);
    }

    #[test]
    fn older_antigravity_invoke_subagent_dynamic_tools_are_backfilled_with_child_threads() {
        use crate::antigravity::child_work::tests::{agent_message_step, generic_invoke_step};
        use crate::test_records::{AT, append, node as node_record, run, thread, with};

        let store = Store::new(Db::open_in_memory().unwrap());
        let userdata = std::env::temp_dir().join(format!("domovoi-repair-ag-sub-{}", uuid::Uuid::new_v4()));
        let profile = crate::antigravity::profile_in(&userdata, "antigravity");
        let conv_dir = profile.acp.join("conversations");
        let child_logs = profile.acp.join("brain/child-sub-1/.system_generated/logs");
        std::fs::create_dir_all(&conv_dir).unwrap();
        std::fs::create_dir_all(&child_logs).unwrap();
        let transcript = child_logs.join("transcript.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                "{\"step_index\":0,\"type\":\"SYSTEM_MESSAGE\",\"status\":\"DONE\",\"content\":\"Render icon A\"}\n",
                "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"tool_calls\":[{\"name\":\"generate_image\",\"args\":{\"ImageName\":\"\\\"icon_a\\\"\"}}]}\n",
                "{\"step_index\":2,\"type\":\"GENERIC\",\"status\":\"DONE\",\"content\":\"Saved /tmp/icon_a.jpg\"}\n",
                "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"content\":\"Finished concept A.\"}\n",
            ),
        )
        .unwrap();
        let canonical = std::fs::canonicalize(&transcript).unwrap();
        let uri = url::Url::from_file_path(&canonical).unwrap().to_string();
        let db = rusqlite::Connection::open(conv_dir.join("sess-sub.db")).unwrap();
        db.execute_batch("CREATE TABLE steps (idx INTEGER, step_type INTEGER, status INTEGER, step_format INTEGER, step_payload BLOB)").unwrap();
        db.execute(
            "INSERT INTO steps VALUES (66, 132, 3, 0, ?1)",
            [generic_invoke_step("call_sub_1", "App Icon Designer A", "Render icon A", "child-sub-1", &uri)],
        )
        .unwrap();
        db.execute("INSERT INTO steps VALUES (72, 101, 3, 0, ?1)", [agent_message_step("child-sub-1", "/tmp/icon_a.jpg")]).unwrap();
        drop(db);

        let o = Origin { driver: Some("antigravity".into()), provider_instance_id: Some("antigravity".into()), command_id: None };
        append(&store, "t-sub", "thread.created", with(thread("t-sub", "p1"), json!({ "providerInstanceId": "antigravity" })), &o).unwrap();
        let pt_id = "provider-thread:provider:antigravity:native-thread:sess-sub";
        append(
            &store,
            "t-sub",
            "provider-thread.updated",
            json!({
                "id": pt_id, "threadId": "t-sub", "driver": "antigravity", "providerInstanceId": "antigravity",
                "status": "active", "createdAt": AT, "updatedAt": AT, "handoffIds": [], "appThreadId": "t-sub",
                "nativeThreadRef": { "driver": "antigravity", "nativeId": "sess-sub", "strength": "strong" }
            }),
            &o,
        )
        .unwrap();
        append(
            &store,
            "t-sub",
            "run.created",
            with(run("t-sub", "r1", "completed"), json!({ "ordinal": 1, "providerInstanceId": "antigravity", "providerThreadId": pt_id, "rootNodeId": "root-1" })),
            &o,
        )
        .unwrap();
        let node_id = "node:provider:antigravity:native-item:call_sub_1";
        let item_id = "turn-item:provider:antigravity:native-item:call_sub_1";
        append(&store, "t-sub", "node.updated", with(node_record("t-sub", node_id, "completed"), json!({ "runId": "r1", "kind": "tool_call" })), &o).unwrap();
        append(
            &store,
            "t-sub",
            "turn-item.updated",
            json!({
                "type": "dynamic_tool", "id": item_id, "threadId": "t-sub", "runId": "r1", "nodeId": node_id,
                "providerThreadId": pt_id,
                "nativeItemRef": { "driver": "antigravity", "nativeId": "call_sub_1", "strength": "strong" },
                "ordinal": 1000010, "status": "completed", "updatedAt": AT, "toolName": "Run invoke_subagent",
                "input": { "Role": "App Icon Designer A", "Prompt": "Render icon A", "TypeName": "image-generator", "Model": "inherit" }
            }),
            &o,
        )
        .unwrap();

        assert_eq!(antigravity_subagents(&store, &userdata), 1);

        let parent = store.load_thread("t-sub").unwrap().unwrap();
        let sub = parent.subagents.get(node_id).expect("subagent entity created");
        assert_eq!(sub.title.as_deref(), Some("App Icon Designer A"));
        assert_eq!(sub.result.as_deref(), Some("/tmp/icon_a.jpg"));
        let child_id = sub.child_thread_id.as_ref().expect("child thread id").as_str().to_owned();
        let TurnItem::Subagent(shown) = parent.turn_items.get(item_id).expect("replaced in place") else { panic!("expected Subagent turn item") };
        assert_eq!((shown.ordinal, shown.title.as_deref(), shown.result.as_deref()), (1000010, Some("App Icon Designer A"), Some("/tmp/icon_a.jpg")));
        assert_eq!(parent.nodes.get(node_id).unwrap().kind, NodeKind::Subagent);

        let child = store.load_thread(&child_id).unwrap().expect("child thread created");
        assert_eq!(child.record.title, "App Icon Designer A");
        assert!(child.turn_items.iter().any(|i| matches!(i, TurnItem::DynamicTool(t) if t.tool_name.as_deref() == Some("generate_image"))));
        assert!(child.messages.iter().any(|m| m.text == "/tmp/icon_a.jpg"));

        // Idempotent: second startup does nothing.
        assert_eq!(antigravity_subagents(&store, &userdata), 0);
        let _ = std::fs::remove_dir_all(&userdata);
    }
}
