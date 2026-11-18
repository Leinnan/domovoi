//! Provider-native subagents as child work (t3code subagent projection):
//! a subagent the agent starts (Claude's `Task`/`Agent` tool, a Codex
//! `spawnAgent`) gets a `subagent` entity and turn item in the parent run
//! and a child thread (lineage `subagent`) holding its prompt, its own
//! messages and tool calls, and its result.

#![expect(clippy::disallowed_types, reason = "a provider tool call's input, and its output kept as the dynamic tool item's open `output`")]

use super::turn::{Shared, Turn, native_ref, node_status, now, spelled, turn_item};
use crate::orchestration::events::enc;
use serde_json::Value;
use std::collections::HashMap;
use domovoi_model::typed::{
    Actor, AppThread, ConversationMessage, CreationSource, EventBody, ExecutionNode, ForkSource, InputIntent, MessageId,
    MessageRole, NodeId, NodeKind, NodeStatus, ParentRelationship, ProviderDriverKind, ProviderInstanceId, Subagent, SubagentItem,
    SubagentOrigin, SubagentStatus, ThreadId, ThreadLineage, TurnItem, TurnItemId, TurnItemStatus,
};

struct Sub {
    child: String,
    entity: Subagent,
    item: Box<SubagentItem>,
    node: ExecutionNode,
    child_root: ExecutionNode,
    next_child_ordinal: u64,
    /// Child items by native id, for updates.
    child_items: HashMap<String, TurnItem>,
}

#[derive(Default)]
pub struct Subagents {
    by_native: HashMap<String, Sub>,
}

/// t3code `subagentThreadTitle`: the task's title or prompt, kept short.
pub fn child_title(parent_title: &str, title: Option<&str>, prompt: &str, ordinal: usize) -> String {
    let detail = title.map(str::trim).filter(|t| !t.is_empty()).or_else(|| Some(prompt.trim()).filter(|p| !p.is_empty()));
    match detail {
        None => format!("{parent_title} subagent {ordinal}"),
        Some(d) if d.chars().count() <= 72 => d.to_owned(),
        Some(d) => format!("{}...", d.chars().take(69).collect::<String>()),
    }
}

fn terminal(status: &SubagentStatus) -> bool {
    matches!(status, SubagentStatus::Completed | SubagentStatus::Failed | SubagentStatus::Cancelled | SubagentStatus::Interrupted)
}

/// The status a provider adapter calls `status`.
fn status_of(status: &str) -> TurnItemStatus {
    spelled(status).unwrap_or(TurnItemStatus::Completed)
}

impl Subagents {
    pub fn knows(&self, native_id: &str) -> bool {
        self.by_native.contains_key(native_id)
    }

    /// A subagent started: entity, node and turn item in the parent run,
    /// and its child thread with the prompt as its first message.
    pub fn start(&mut self, turn: &mut Turn, native_id: &str, prompt: &str, title: Option<&str>, model: Option<&str>) {
        if self.knows(native_id) {
            return;
        }
        // The child is a thread of its own made from the parent's record.
        let Ok(Some(parent)) = turn.store().app_thread(&turn.thread_id) else {
            tracing::error!(thread = %turn.thread_id, "no thread record to start a subagent's thread from");
            return;
        };
        let ts = now();
        let child = format!("{}:subagent:{}", turn.thread_id, enc(native_id));
        let (thread_id, child_id) = (ThreadId::from(turn.thread_id.as_str()), ThreadId::from(child.as_str()));
        let node_id = NodeId::from(format!("node:provider:{}:native-item:{}", turn.driver, enc(native_id)));
        let ordinal = self.by_native.len() + 1;
        let thread = AppThread {
            id: child_id.clone(),
            title: child_title(&parent.title, title, prompt, ordinal),
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
            pull_requests: Some(Vec::new()),
            lineage: ThreadLineage {
                parent_thread_id: Some(thread_id.clone()),
                relationship_to_parent: Some(ParentRelationship::Subagent),
                root_thread_id: parent.lineage.root_thread_id.clone(),
                side_chat: None,
            },
            forked_from: Some(ForkSource::Node { node_id: node_id.clone() }),
            created_at: ts.clone(),
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
            ..parent
        };
        turn.emit_on(&child, EventBody::Thread { change: domovoi_model::typed::ThreadChange::Created, thread: Box::new(thread) });

        let native = native_ref(&turn.driver, native_id);
        let (driver, instance) = (ProviderDriverKind::from(turn.driver.as_str()), ProviderInstanceId::from(turn.instance.as_str()));
        let node = ExecutionNode {
            id: node_id.clone(),
            thread_id: thread_id.clone(),
            run_id: Some(turn.run.id.clone()),
            parent_node_id: Some(turn.root_node().clone()),
            root_node_id: turn.root_node().clone(),
            kind: NodeKind::Subagent,
            status: NodeStatus::Running,
            counts_for_run: false,
            provider_thread_id: Some(turn.provider_thread_id().clone()),
            provider_turn_id: turn.provider_turn_id().cloned(),
            native_item_ref: Some(native.clone()),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: Some(ts.clone()),
            completed_at: None,
        };
        turn.emit_on(&turn.thread_id, EventBody::NodeUpdated(Box::new(node.clone())));
        let entity = Subagent {
            id: node_id.clone(),
            thread_id: thread_id.clone(),
            run_id: Some(turn.run.id.clone()),
            parent_node_id: turn.root_node().clone(),
            origin: SubagentOrigin::ProviderNative,
            created_by: Actor::Agent,
            driver: driver.clone(),
            provider_instance_id: instance.clone(),
            provider_thread_id: Some(turn.provider_thread_id().clone()),
            child_thread_id: Some(child_id.clone()),
            remote_child: None,
            native_task_ref: Some(native.clone()),
            prompt: prompt.to_owned(),
            title: title.map(str::to_owned),
            model: model.map(str::to_owned),
            model_selection: None,
            completion_wake: None,
            completion_delivery: None,
            cancelled_run_id: None,
            status: SubagentStatus::Running,
            progress: None,
            workflow: None,
            result: None,
            started_at: Some(ts.clone()),
            completed_at: None,
            updated_at: ts.clone(),
            prompt_omitted: None,
            result_omitted: None,
        };
        turn.emit_on(&turn.thread_id, EventBody::SubagentUpdated(Box::new(entity.clone())));
        let child_root_id = NodeId::from(format!("node:subagent-root:{}", enc(&child)));
        let child_root = ExecutionNode {
            id: child_root_id.clone(),
            thread_id: child_id.clone(),
            run_id: None,
            parent_node_id: None,
            root_node_id: child_root_id,
            kind: NodeKind::RootTurn,
            status: NodeStatus::Running,
            counts_for_run: false,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: Some(native.clone()),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: Some(ts.clone()),
            completed_at: None,
        };
        turn.emit_on(&child, EventBody::NodeUpdated(Box::new(child_root.clone())));
        let shared = Shared {
            id: TurnItemId::from(format!("turn-item:provider:{}:native-item:{}", turn.driver, enc(native_id))),
            thread_id,
            run_id: Some(turn.run.id.clone()),
            node_id: Some(node_id.clone()),
            provider_thread_id: Some(turn.provider_thread_id().clone()),
            provider_turn_id: turn.provider_turn_id().cloned(),
            native_item_ref: Some(native),
            parent_item_id: None,
            ordinal: turn.alloc_ordinal(),
            status: TurnItemStatus::Running,
            title: title.map(str::to_owned),
            started_at: Some(ts.clone()),
            completed_at: None,
            updated_at: ts,
        };
        let TurnItem::Subagent(item) = turn_item!(Subagent(SubagentItem), shared, {
            subagent_id: node_id,
            origin: SubagentOrigin::ProviderNative,
            driver: driver,
            provider_instance_id: instance,
            child_thread_id: Some(child_id),
            remote_child: None,
            prompt: prompt.to_owned(),
            progress: None,
            result: None,
            prompt_omitted: None,
            result_omitted: None,
        }) else {
            return;
        };
        turn.emit_on(&turn.thread_id, EventBody::TurnItemUpdated(TurnItem::Subagent(item.clone())));
        let mut sub = Sub { child, entity, item, node, child_root, next_child_ordinal: 100, child_items: HashMap::new() };
        self.message(turn, &mut sub, &format!("{native_id}:prompt"), MessageRole::User, prompt);
        self.by_native.insert(native_id.to_owned(), sub);
    }

    /// The shared fields of the child thread's item for `native_id`, moved to
    /// `status`: the item's as already written, or a new one's at the next ordinal.
    fn child_item_shared(sub: &mut Sub, native_id: &str, status: TurnItemStatus, driver: &str) -> Shared {
        let shared = match sub.child_items.get(native_id) {
            Some(item) => Shared::of(item),
            None => {
                let ts = now();
                let ordinal = sub.next_child_ordinal;
                sub.next_child_ordinal += 1;
                Shared {
                    id: TurnItemId::from(format!("turn-item:subagent:{}:{}", enc(&sub.child), enc(native_id))),
                    thread_id: ThreadId::from(sub.child.as_str()),
                    run_id: None,
                    node_id: Some(sub.child_root.id.clone()),
                    provider_thread_id: None,
                    provider_turn_id: None,
                    native_item_ref: Some(native_ref(driver, native_id)),
                    parent_item_id: None,
                    ordinal,
                    status: status.clone(),
                    title: None,
                    started_at: Some(ts.clone()),
                    completed_at: None,
                    updated_at: ts,
                }
            }
        };
        shared.at(status)
    }

    fn message(&self, turn: &Turn, sub: &mut Sub, native_id: &str, role: MessageRole, text: &str) {
        let ts = now();
        let message_id = MessageId::from(format!("message:subagent:{}:{}", enc(&sub.child), enc(native_id)));
        let from_user = role == MessageRole::User;
        // The subagent's prompt and replies both come from the provider.
        turn.emit_on(&sub.child, EventBody::MessageUpdated(Box::new(ConversationMessage {
            notification: None,
            created_by: Actor::Agent,
            creation_source: CreationSource::Provider,
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            workflow_script: None,
            id: message_id.clone(),
            thread_id: ThreadId::from(sub.child.as_str()),
            run_id: None,
            node_id: Some(sub.child_root.id.clone()),
            role,
            text: text.to_owned(),
            context: None,
            attachments: Vec::new(),
            streaming: false,
            created_at: ts.clone(),
            updated_at: ts,
            delegated_completion: None,
        })));
        let shared = Self::child_item_shared(sub, native_id, TurnItemStatus::Completed, &turn.driver);
        let item = if from_user {
            turn_item!(UserMessage(UserMessageItem), shared, {
                created_by: Actor::Agent,
                creation_source: CreationSource::Provider,
                message_id: message_id,
                scheduled_task_id: None,
                sender_thread_id: Some(ThreadId::from(turn.thread_id.as_str())),
                linked_sender: None,
                idle_compaction: None,
                workflow_script: None,
                fresh_context: None,
                input_intent: InputIntent::TurnStart,
                text: text.to_owned(),
                text_omitted: None,
                context: None,
                attachments: Vec::new(),
                model_switch: None,
            })
        } else {
            turn_item!(AssistantMessage(AssistantMessageItem), shared, {
                phase: None,
                message_id: message_id,
                text: text.to_owned(),
                text_omitted: None,
                attachments: None,
                streaming: false,
            })
        };
        sub.child_items.insert(native_id.to_owned(), item.clone());
        turn.emit_on(&sub.child, EventBody::TurnItemUpdated(item));
    }

    /// The subagent's own reply text.
    pub fn text(&mut self, turn: &Turn, owner: &str, native_id: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let Some(mut sub) = self.by_native.remove(owner) else { return };
        self.message(turn, &mut sub, native_id, MessageRole::Assistant, text);
        self.by_native.insert(owner.to_owned(), sub);
    }

    /// A tool the subagent called (or its result), shown in its thread.
    #[expect(clippy::too_many_arguments, reason = "one call per tool event, each argument a part of it")]
    pub fn tool(&mut self, turn: &Turn, owner: &str, native_id: &str, name: &str, input: &Value, output: Option<&str>, status: &str) {
        let Some(sub) = self.by_native.get_mut(owner) else { return };
        let shared = Self::child_item_shared(sub, native_id, status_of(status), &turn.driver);
        let item = if name == "Bash" {
            let previous = match sub.child_items.get(native_id) {
                Some(TurnItem::CommandExecution(previous)) => previous.output.clone(),
                _ => None,
            };
            turn_item!(CommandExecution(CommandExecutionItem), shared, {
                input: input["command"].as_str().unwrap_or_default().to_owned(),
                output: output.map(str::to_owned).or(previous),
                output_omitted: None,
                output_indicates_failure: None,
                exit_code: None,
                monitor: None,
                completion_wake: None,
            })
        } else {
            let previous = match sub.child_items.get(native_id) {
                Some(TurnItem::DynamicTool(previous)) => previous.output.clone(),
                _ => None,
            };
            turn_item!(DynamicTool(DynamicToolItem), shared, {
                tool_name: Some(name.to_owned()),
                viewed_image_path: None,
                input: Some(input.clone()),
                output: output.map(|o| Value::String(o.to_owned())).or(previous),
                output_omitted: None,
            })
        };
        sub.child_items.insert(native_id.to_owned(), item.clone());
        turn.emit_on(&sub.child, EventBody::TurnItemUpdated(item));
    }

    /// Progress text (Claude `task_progress`).
    pub fn progress(&mut self, turn: &Turn, native_id: &str, progress: &str) {
        let Some(sub) = self.by_native.get_mut(native_id) else { return };
        sub.entity.progress = Some(progress.to_owned());
        sub.entity.updated_at = now();
        sub.item.progress = Some(progress.to_owned());
        turn.emit_on(&turn.thread_id, EventBody::SubagentUpdated(Box::new(sub.entity.clone())));
        turn.emit_on(&turn.thread_id, EventBody::TurnItemUpdated(TurnItem::Subagent(sub.item.clone())));
    }

    /// The subagent's thread, once it has one.
    pub fn child_of(&self, native_id: &str) -> Option<String> {
        self.by_native.get(native_id).map(|s| s.child.clone())
    }

    /// The subagent ended: `completed`, `failed`, `cancelled` or `interrupted`.
    pub fn finish(&mut self, turn: &Turn, native_id: &str, status: &str, result: Option<&str>) {
        let Some(mut sub) = self.by_native.remove(native_id) else { return };
        if terminal(&sub.entity.status) {
            self.by_native.insert(native_id.to_owned(), sub);
            return;
        }
        let ts = now();
        if let Some(r) = result.map(str::trim).filter(|r| !r.is_empty()) {
            self.message(turn, &mut sub, &format!("{native_id}:result"), MessageRole::Assistant, r);
        }
        let status = status_of(status);
        sub.entity.status = spelled(status.as_str()).unwrap_or(SubagentStatus::Completed);
        sub.entity.completed_at = Some(ts.clone());
        sub.entity.updated_at = ts.clone();
        sub.entity.result = result.map(str::to_owned);
        sub.item.status = status.clone();
        sub.item.completed_at = Some(ts.clone());
        sub.item.updated_at = ts.clone();
        sub.item.result = result.map(str::to_owned);
        for node in [&mut sub.node, &mut sub.child_root] {
            node.status = node_status(&status);
            node.completed_at = Some(ts.clone());
        }
        turn.emit_on(&turn.thread_id, EventBody::NodeUpdated(Box::new(sub.node.clone())));
        turn.emit_on(&turn.thread_id, EventBody::SubagentUpdated(Box::new(sub.entity.clone())));
        turn.emit_on(&turn.thread_id, EventBody::TurnItemUpdated(TurnItem::Subagent(sub.item.clone())));
        turn.emit_on(&sub.child, EventBody::NodeUpdated(Box::new(sub.child_root.clone())));
        self.by_native.insert(native_id.to_owned(), sub);
    }

    /// The turn ended: subagents still running stop with it.
    pub fn end(&mut self, turn: &Turn, completed: bool) {
        let open: Vec<String> = self.by_native.iter().filter(|(_, s)| !terminal(&s.entity.status)).map(|(k, _)| k.clone()).collect();
        for k in open {
            self.finish(turn, &k, if completed { "completed" } else { "interrupted" }, None);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn child_titles_are_the_task_or_its_prompt_kept_short() {
        assert_eq!(super::child_title("Parent", Some("Find the bug"), "long prompt", 1), "Find the bug");
        assert_eq!(super::child_title("Parent", None, "", 2), "Parent subagent 2");
        let long = "x".repeat(100);
        assert_eq!(super::child_title("P", None, &long, 1).chars().count(), 72);
    }
}
