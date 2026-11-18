//! The view model, derived from the model on every render.

#![expect(clippy::disallowed_types, reason = "fields the schema leaves open: a tool call's input and output, an answer to a question, an item of a type this build does not know")]

use crate::app::age_label;
use crate::model::{BuiltRow, Connection, Model};
use crate::replies::shell_status;
use crate::settings;
use crate::sidebar::{is_pinned, is_settled, relation, sidebar};
use crate::update::{self, composer_interaction_mode, composer_runtime_mode, current_effort, current_model, effort_descriptor, model_options, steerable_run};
use crate::view::*;
use crate::{diff, markdown};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use domovoi_model::typed::{
    AuthClientDeviceType, BackgroundTaskKind, ChatAttachment, CheckpointFileSummary, CheckpointId, CheckpointStatus, ConversationMessage,
    FileChangeItem, KeybindingWhenNode, PendingBackgroundTask, PlanArtifact, PlanStatus, PlanStepStatus, ProjectEntry,
    ProjectEntryKind, ProviderThread, PullRequestListEntry, ResolvedKeybindingRule, Run, RunId, RunStatus, RuntimeRequestId,
    RuntimeRequestKind, RuntimeRequestStatus, ScheduledTaskRunStatus, ScheduledTaskSchedule, ServerAuthSessionMethod, ShellStatus,
    SubagentStatus, Thread, ThreadShell, Timestamp, TurnItem, TurnItemStatus, UsagePricingStatus, UserInputRequestItem,
};

/// A count the server sends, as the views carry counts.
fn narrow(n: u64) -> u32 {
    n as u32
}

pub fn monogram(title: &str) -> String {
    let words: Vec<&str> = title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let m: String = words.iter().take(2).filter_map(|w| w.chars().next()).collect::<String>().to_uppercase();
    if m.is_empty() { "?".into() } else { m }
}

pub fn bytes_label(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

pub fn count_label(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Captured checkpoints of the thread, oldest first.
pub fn checkpoints(thread: &Thread) -> Vec<CheckpointView> {
    // A checkpoint that lists no files takes them from its row in the timeline.
    let mut rows: Option<FxHashMap<&CheckpointId, &[CheckpointFileSummary]>> = None;
    let mut out: Vec<CheckpointView> = thread
        .checkpoints
        .iter()
        .filter(|c| c.ordinal_within_scope > 0 && !matches!(c.status, CheckpointStatus::Stale | CheckpointStatus::Error))
        .map(|c| {
            let files: &[CheckpointFileSummary] = if c.files.is_empty() {
                rows.get_or_insert_with(|| checkpoint_rows(thread)).get(&c.id).copied().unwrap_or_default()
            } else {
                &c.files
            };
            let n = c.ordinal_within_scope as u32;
            CheckpointView {
                id: c.id.as_str().to_owned(),
                scope_id: c.scope_id.as_str().to_owned(),
                turn_count: n,
                label: format!("Turn {n}"),
                files_changed: files.len() as u32,
                additions: files.iter().fold(0u32, |sum, f| sum.saturating_add(f.additions as u32)),
                deletions: files.iter().fold(0u32, |sum, f| sum.saturating_add(f.deletions as u32)),
                can_revert: true,
            }
        })
        .collect();
    out.sort_by_key(|c| c.turn_count);
    out
}

/// The files each checkpoint's first row in the timeline lists.
fn checkpoint_rows(thread: &Thread) -> FxHashMap<&CheckpointId, &[CheckpointFileSummary]> {
    let mut rows = FxHashMap::default();
    for item in &thread.turn_items {
        if let TurnItem::Checkpoint(row) = item {
            rows.entry(&row.checkpoint_id).or_insert(row.files.as_slice());
        }
    }
    rows
}

/// The thread's timeline as shown, each item as the thread shares it (a row
/// is kept with the records it was built from).
fn shown_items(thread: &Thread) -> Vec<&Arc<TurnItem>> {
    let all = thread.turn_items.as_slice();
    let mut next = 0;
    thread
        .visible_turn_item_refs()
        .into_iter()
        .filter_map(|item| {
            // The items shown are the thread's own, in its order: each is
            // found a little after the one before.
            match all[next..].iter().position(|held| std::ptr::eq(&**held, item)) {
                Some(offset) => {
                    next += offset + 1;
                    all.get(next - 1)
                }
                None => thread.turn_items.shared(item.id()),
            }
        })
        .collect()
}

fn attachment_name(attachment: &ChatAttachment) -> &str {
    match attachment {
        ChatAttachment::Image(image) => &image.name,
        ChatAttachment::File(file) => &file.name,
        ChatAttachment::Other(other) => &other.name,
    }
}

/// Long command output and tool input show their last lines until expanded.
const OUTPUT_LINES: usize = 200;
const OUTPUT_BYTES: usize = 16 * 1024;

/// The end of `text` within the limits, and whether anything was cut.
fn tail(text: &str) -> (String, bool) {
    let mut start = text.len().saturating_sub(OUTPUT_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut cut = &text[start..];
    let lines = cut.lines().count();
    if lines > OUTPUT_LINES {
        let skip = lines - OUTPUT_LINES;
        cut = cut.splitn(skip + 1, '\n').last().unwrap_or(cut);
    }
    if cut.len() == text.len() { (text.to_owned(), false) } else { (format!("…\n{cut}"), true) }
}

impl BuiltRow {
    /// Whether the row was built from exactly this: these very records (a
    /// changed record is another one), this view of its checkpoint, and its
    /// output cut or in full as now.
    fn is_of(&self, item: &Arc<TurnItem>, message: Option<&Arc<ConversationMessage>>, checkpoint: Option<&CheckpointView>, expanded: bool) -> bool {
        let same_message = match (&self.message, message) {
            (Some(built_from), Some(message)) => Arc::ptr_eq(built_from, message),
            (None, None) => true,
            _ => false,
        };
        Arc::ptr_eq(&self.item, item) && same_message && self.expanded == expanded && self.row.checkpoint.as_ref() == checkpoint
    }
}

/// A `user_input_request` item's questions.
fn questions(item: &UserInputRequestItem) -> Vec<QuestionView> {
    item.questions
        .iter()
        .map(|q| QuestionView {
            id: q.id.clone(),
            header: q.header.clone(),
            question: q.question.clone(),
            multi_select: q.multi_select == Some(true),
            options: q.options.iter().map(|o| QuestionOptionView { label: o.label.clone(), description: o.description.clone() }).collect(),
        })
        .collect()
}

/// The file a `file_change` item edits.
fn file_change_path(change: &FileChangeItem) -> String {
    change.changes.as_ref().and_then(|changes| changes.first()).map_or_else(|| change.file_name.clone(), |first| first.path.clone())
}

/// A `file_change` item's edit as a diff: old and new text (Claude's Edit),
/// or a unified patch.
fn file_change_diff(change: &FileChangeItem) -> Option<DiffFileView> {
    match (&change.old_str, &change.new_str) {
        (Some(old), Some(new)) => Some(diff::diff_texts(&file_change_path(change), old, new)),
        _ => change.diff_str.as_deref().and_then(|patch| diff::parse_unified(patch).into_iter().next()),
    }
}

/// Every file a `file_change` item edits, as diffs (a Codex patch can hold
/// several).
fn file_change_diffs(change: &FileChangeItem) -> Vec<DiffFileView> {
    match &change.diff_str {
        Some(patch) if change.old_str.is_none() => diff::parse_unified(patch),
        _ => file_change_diff(change).into_iter().collect(),
    }
}

/// One answer as text: several choices joined with ", ". (The schema leaves
/// an answer's shape open.)
fn answer_text(a: &Value) -> String {
    match a {
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// A tool call's row: its name, and its input and output as text (each tool
/// defines their shape), cut to their ends until the row is expanded.
fn tool_row(v: &mut ItemView, name: Option<&str>, input: Option<&Value>, output: Option<&Value>, expanded: bool) {
    v.kind = "tool".into();
    v.title = name.unwrap_or("Tool").to_owned();
    let input = match input {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let output = match output {
        Some(Value::String(text)) => Some(text.clone()),
        None | Some(Value::Null) => None,
        Some(other) => Some(other.to_string()),
    };
    if expanded {
        v.text = input;
        v.output = output.map(Into::into);
    } else {
        let (text, a) = tail(&input);
        let (out, b) = output.as_deref().map(tail).map_or((None, false), |(o, c)| (Some(o), c));
        v.text = text;
        v.output = out.map(Into::into);
        v.truncated = a || b;
    }
    v.collapsed = true;
}

/// A plan's row: its markdown.
fn plan_row(v: &mut ItemView, plan_id: &str, markdown: &str) {
    v.kind = "plan".into();
    v.title = "Plan".into();
    v.plan_id = plan_id.to_owned();
    v.text = markdown.to_owned();
    v.blocks = markdown::parse(&v.text).into();
}

/// The row of an item that is neither conversation nor a step with a look
/// of its own: its title, or else its type in words.
fn notice_row(v: &mut ItemView, item: &TurnItem, text: &str) {
    v.kind = "notice".into();
    v.title = item.title().filter(|t| !t.is_empty()).map(str::to_owned).unwrap_or_else(|| item.item_type().replace('_', " "));
    v.text = text.to_owned();
}

/// The row of one turn item. `message` is the message the item shows, when
/// the thread holds it; `checkpoint` the view of the checkpoint a checkpoint
/// item stands for.
fn item_view(item: &TurnItem, message: Option<&ConversationMessage>, checkpoint: Option<&CheckpointView>, expanded: bool, prefixes: &mut FxHashMap<String, markdown::Prefix>) -> ItemView {
    let mut v = ItemView { id: item.id().as_str().to_owned(), status: item.status().as_str().to_owned(), ..Default::default() };
    let completed = *item.status() == TurnItemStatus::Completed;
    match item {
        TurnItem::UserMessage(i) => {
            v.kind = "user".into();
            // The message holds the text; the item's own copy stands in for a message not held.
            let (text, attachments) = message.map_or((&i.text, &i.attachments), |m| (&m.text, &m.attachments));
            v.text = text.clone();
            v.attachments = attachments.iter().map(|a| attachment_name(a).to_owned()).collect();
        }
        TurnItem::AssistantMessage(i) => {
            v.kind = "assistant".into();
            v.text = message.map_or(&i.text, |m| &m.text).clone();
            v.streaming = i.streaming || message.is_some_and(|m| m.streaming);
            // A streaming reply grows at its end: only the part after its
            // settled prefix is parsed again.
            if v.streaming {
                let (blocks, prefix) = markdown::parse_streaming(prefixes.get(&v.id), &v.text);
                v.blocks = blocks.into();
                match prefix {
                    Some(p) => prefixes.insert(v.id.clone(), p),
                    None => prefixes.remove(&v.id),
                };
            } else {
                prefixes.remove(&v.id);
                v.blocks = markdown::parse(&v.text).into();
            }
        }
        TurnItem::Reasoning(i) => {
            v.kind = "reasoning".into();
            v.title = "Thinking".into();
            v.text = i.text.clone();
            v.collapsed = true;
        }
        TurnItem::CommandExecution(i) => {
            v.kind = "command".into();
            v.title = i.input.clone();
            if let Some(out) = &i.output {
                let (shown, cut) = if expanded { (out.clone(), false) } else { tail(out) };
                v.output = Some(shown.into());
                v.truncated = cut;
            }
            v.exit_code = i.exit_code;
            v.collapsed = completed;
        }
        TurnItem::FileChange(i) => {
            v.kind = "file_change".into();
            v.title = file_change_path(i);
            v.diff = file_change_diff(i).map(Arc::new);
            v.collapsed = completed;
        }
        TurnItem::TodoList(i) => {
            v.kind = "todo".into();
            v.title = i.explanation.as_deref().filter(|e| !e.trim().is_empty()).unwrap_or("To-dos").to_owned();
            v.todos = i
                .steps
                .iter()
                .map(|step| TodoView {
                    text: step.text.clone(),
                    status: match &step.status {
                        PlanStepStatus::Completed => "completed".into(),
                        PlanStepStatus::Running => "in_progress".into(),
                        PlanStepStatus::Other(status) if &**status == "in_progress" => "in_progress".into(),
                        PlanStepStatus::Pending | PlanStepStatus::Other(_) => "pending".into(),
                    },
                })
                .collect::<Vec<_>>()
                .into();
        }
        TurnItem::Subagent(i) => {
            v.kind = "subagent".into();
            v.title = i.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("Subagent").to_owned();
            v.text = i.prompt.clone();
            v.output = i.result.as_deref().filter(|r| !r.trim().is_empty()).map(|r| tail(r).0.into());
            v.child_thread_id = i.child_thread_id.as_ref().map(|id| id.as_str().to_owned()).unwrap_or_default();
            v.collapsed = true;
        }
        // Claude's to-do tool: its input is the list.
        TurnItem::DynamicTool(i) if i.tool_name.as_deref() == Some("TodoWrite") => {
            v.kind = "todo".into();
            v.title = "To-dos".into();
            v.todos = i
                .input
                .as_ref()
                .and_then(|input| input.get("todos"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| {
                    let text = |key: &str| t.get(key).and_then(Value::as_str);
                    TodoView { text: text("content").or(text("activeForm")).unwrap_or_default().to_owned(), status: text("status").unwrap_or_default().to_owned() }
                })
                .collect::<Vec<_>>()
                .into();
        }
        TurnItem::DynamicTool(i) => tool_row(&mut v, i.tool_name.as_deref().or(i.title.as_deref()), i.input.as_ref(), i.output.as_ref(), expanded),
        TurnItem::WebSearch(i) => tool_row(&mut v, i.title.as_deref(), None, None, expanded),
        TurnItem::UserInputRequest(i) => {
            v.kind = "question".into();
            v.title = i.questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n");
        }
        TurnItem::ApprovalRequest(i) => {
            v.kind = "approval".into();
            v.title = i.prompt.as_deref().unwrap_or("Approval requested").to_owned();
            v.text = i.request_kind.as_str().to_owned();
        }
        TurnItem::ProposedPlan(i) => plan_row(&mut v, i.plan_id.as_str(), &i.markdown),
        TurnItem::Checkpoint(i) => {
            v.kind = "checkpoint".into();
            v.checkpoint = checkpoint.cloned();
            v.title = match i.files.len() {
                0 => "No file changes".into(),
                1 => "1 file changed".into(),
                n => format!("{n} files changed"),
            };
        }
        TurnItem::Error(i) => {
            v.kind = "error".into();
            v.title = i.failure.message.clone();
        }
        // An item of a type this build does not know keeps its record as it
        // was sent. Two such types have a look of their own, a tool call and
        // a plan under names the schema does not list; any other is a notice
        // with its text.
        TurnItem::Other(unknown) => {
            let field = |key: &str| unknown.record().get(key);
            let text = |key: &str| field(key).and_then(Value::as_str);
            match unknown.item_type() {
                "mcp_tool_call" => tool_row(&mut v, text("toolName").or(item.title()), field("input"), field("output"), expanded),
                "plan" => plan_row(&mut v, text("planId").unwrap_or_default(), text("markdown").or(text("text")).unwrap_or_default()),
                _ => notice_row(&mut v, item, text("text").unwrap_or_default()),
            }
        }
        TurnItem::Notification(_)
        | TurnItem::FileSearch(_)
        | TurnItem::RunInterruptRequest(_)
        | TurnItem::RunInterruptResult(_)
        | TurnItem::SystemNotice(_)
        | TurnItem::Compaction(_)
        | TurnItem::Handoff(_)
        | TurnItem::Fork(_)
        | TurnItem::ThreadCreated(_)
        | TurnItem::SecretRequest(_)
        | TurnItem::LinkRequest(_) => notice_row(&mut v, item, ""),
    }
    v
}

/// A row of the open thread built again from its item, when the item's text
/// grew and nothing else changed (`Changes::row`): `old` is the row as last
/// built. `None` when the item is gone.
pub(crate) fn regrown_row(model: &Model, item_id: &str, old: &ItemView) -> Option<ItemView> {
    let thread = model.thread.as_ref()?;
    let item = thread.turn_items.get(item_id)?;
    let message = item.message_id().and_then(|id| thread.messages.get(id));
    let mut prefixes = model.markdown_prefixes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // The rows whose text grows (a reply, thinking, a command) show no checkpoint.
    let mut row = item_view(item, message, None, model.expanded.contains(item_id), &mut prefixes);
    carry_context(old, &mut row);
    Some(row)
}

/// What `thread_page` writes on a row from where it stands among the others
/// (the search, its turn, the steps around it), not from its own item: kept
/// when the row alone is built again. A row whose text grew keeps its place.
fn carry_context(old: &ItemView, row: &mut ItemView) {
    row.find_matches = old.find_matches;
    row.find_active = old.find_active;
    row.can_retry = old.can_retry;
    row.group.clone_from(&old.group);
    row.group_summary.clone_from(&old.group_summary);
    row.run_started_ms = old.run_started_ms;
    row.run_duration_ms = old.run_duration_ms;
}

/// Rows that are steps of the agent's work rather than conversation.
fn is_step(kind: &str) -> bool {
    matches!(kind, "command" | "file_change" | "tool" | "reasoning")
}

/// Folds two or more consecutive steps into a group, summarized on its
/// first row.
fn group_steps(items: &mut [ItemView]) {
    let mut i = 0;
    while i < items.len() {
        if !is_step(&items[i].kind) {
            i += 1;
            continue;
        }
        let start = i;
        let (mut commands, mut edits, mut tools) = (0usize, 0usize, 0usize);
        while i < items.len() && is_step(&items[i].kind) {
            match items[i].kind.as_str() {
                "command" => commands += 1,
                "file_change" => edits += 1,
                "tool" => tools += 1,
                _ => {}
            }
            i += 1;
        }
        if i - start < 2 {
            continue;
        }
        // "Ran 2 commands · edited 1 file · used 3 tools", or of thinking alone "Thought it over".
        let mut summary = String::new();
        for (count, did, one, many) in [(commands, "ran", "command", "commands"), (edits, "edited", "file", "files"), (tools, "used", "tool", "tools")] {
            if count > 0 {
                if !summary.is_empty() {
                    summary.push_str(" · ");
                }
                summary.push_str(did);
                summary.push(' ');
                summary.push_str(&count.to_string());
                summary.push(' ');
                summary.push_str(if count == 1 { one } else { many });
            }
        }
        if summary.is_empty() {
            summary.push_str("thought it over");
        }
        if let Some(first) = summary.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        let group = items[start].id.clone();
        for row in &mut items[start..i] {
            row.group.clone_from(&group);
        }
        items[start].group_summary = summary;
    }
}

/// A user message's images whose URLs have come.
fn images(model: &Model, message: &ConversationMessage) -> Vec<ImageView> {
    message
        .attachments
        .iter()
        .filter_map(|attachment| {
            let ChatAttachment::Image(image) = attachment else { return None };
            let url = model.asset_urls.get(&image.id).filter(|u| !u.is_empty())?;
            Some(ImageView { name: image.name.clone(), url: format!("{}{url}", model.server_url) })
        })
        .collect()
}

/// The request a turn item asks the user to answer.
fn request_of(item: &TurnItem) -> Option<&RuntimeRequestId> {
    match item {
        TurnItem::ApprovalRequest(approval) => Some(&approval.request_id),
        TurnItem::UserInputRequest(question) => Some(&question.request_id),
        _ => None,
    }
}

/// The thread's queued runs, in the order they run.
fn queued_runs(thread: &Thread) -> Vec<&Run> {
    let mut queued: Vec<&Run> = thread.runs.iter().filter(|r| r.status == RunStatus::Queued).collect();
    queued.sort_by_key(|r| (r.queue_position.flatten().unwrap_or(u64::MAX), r.ordinal));
    queued
}

fn thread_page(model: &Model) -> Option<ThreadPageView> {
    let tid = model.selected_thread.as_deref()?;
    let shell = model.thread_shell(tid);
    let status = shell.map_or(ShellStatus::Idle, shell_status);
    let project_title = shell.and_then(|shell| model.project(shell.project_id.as_str())).map(|p| p.title.clone()).unwrap_or_default();
    let model_label = current_model(model).map(|m| m.label).unwrap_or_default();
    let mut page = ThreadPageView {
        id: tid.to_owned(),
        title: shell.map(|shell| shell.title.clone()).unwrap_or_default(),
        project_id: shell.map(|shell| shell.project_id.as_str().to_owned()).unwrap_or_default(),
        project_title,
        branch: shell.and_then(|shell| shell.branch.clone()).filter(|branch| !branch.is_empty()),
        worktree_path: shell.and_then(|shell| shell.worktree_path.clone()).filter(|path| !path.is_empty()),
        cwd: model.thread_cwd(tid).unwrap_or_default(),
        model_label,
        provider_instance_id: shell.map(|shell| shell.provider_instance_id.as_str().to_owned()).unwrap_or_default(),
        runtime_mode: composer_runtime_mode(model),
        interaction_mode: composer_interaction_mode(model),
        status: status.as_str().into(),
        working: matches!(status, ShellStatus::Working | ShellStatus::Approval),
        loading: model.thread.is_none() || model.thread_loading,
        pinned: shell.is_some_and(is_pinned),
        settled: shell.is_some_and(is_settled),
        archived: shell.is_some_and(|shell| shell.archived_at.is_some()),
        ..Default::default()
    };
    page.pending_sends = model
        .composer
        .outbox
        .iter()
        .filter(|o| o.draft_key == tid && !o.text.is_empty())
        // Already in the thread (its echo came before the answer).
        .filter(|o| !o.message_id.as_deref().is_some_and(|m| model.thread.as_ref().is_some_and(|t| t.messages.contains(m))))
        .map(|o| PendingSendView {
            // Unique per row: the shells key rows by it.
            id: match &o.route {
                crate::rpc::Route::Send(id) | crate::rpc::Route::Launch(id) => id.clone(),
                _ => o.message_id.clone().unwrap_or_else(|| format!("{}-{}", o.label, o.sent_ms)),
            },
            text: o.text.clone(),
            state: if model.connection == Connection::Connected { "sending".into() } else { "waiting".into() },
        })
        .collect();
    page.parent = shell.and_then(|shell| Some((shell, shell.lineage.parent_thread_id.as_ref()?))).map(|(shell, parent)| ThreadLinkView {
        thread_id: parent.as_str().to_owned(),
        title: model.thread_shell(parent.as_str()).map(|t| t.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Earlier thread".into()),
        relation: relation(shell).to_owned(),
    });
    let Some(thread) = &model.thread else { return Some(page) };
    let cps = checkpoints(thread);
    let checkpoint_views: FxHashMap<&str, &CheckpointView> = cps.iter().map(|c| (c.id.as_str(), c)).collect();
    let find = model.find.result.as_ref();
    let counts: HashMap<&str, u32> = find.and_then(|found| found.navigation.as_ref()).into_iter().flatten().map(|n| (n.entry_id.as_str(), narrow(n.count))).collect();
    let active_entry = find.and_then(|found| found.active_match.as_ref()).map(|active| active.entry_id.as_str());
    let mut active_item = None;
    let visible = shown_items(thread);
    let runs_with = |wanted: fn(&TurnItem) -> bool| -> FxHashSet<&RunId> { visible.iter().filter(|i| wanted(i)).filter_map(|i| i.run_id()).collect() };
    // A run that failed and changed nothing gets no "No file changes" card.
    let failed_runs = runs_with(|i| matches!(i, TurnItem::Error(_)));
    // A turn whose to-dos came as a list doesn't show the tool call too.
    let todo_runs = runs_with(|i| matches!(i, TurnItem::TodoList(_)));
    let in_runs = |item: &TurnItem, runs: &FxHashSet<&RunId>| item.run_id().is_some_and(|r| runs.contains(r));
    let shown: Vec<&Arc<TurnItem>> = visible
        .into_iter()
        .filter(|item| match &***item {
            TurnItem::Checkpoint(c) => !(c.files.is_empty() && in_runs(item, &failed_runs)),
            TurnItem::DynamicTool(tool) => !(tool.tool_name.as_deref() == Some("TodoWrite") && in_runs(item, &todo_runs)),
            _ => true,
        })
        .collect();
    let last_error = shown.last().filter(|i| matches!(&****i, TurnItem::Error(_))).map(|i| i.id());
    let mut cache = model.item_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut prefixes = model.markdown_prefixes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Rows of items that are no longer shown stay behind: start over now and then.
    if cache.len() > 4 * shown.len() + 2000 {
        cache.clear();
        prefixes.clear();
    }
    page.items = shown
        .iter()
        .map(|item| {
            let id = item.id();
            let expanded = model.expanded.contains(id.as_str());
            let message = item.message_id().and_then(|m| thread.messages.shared(m));
            let checkpoint = match &***item {
                TurnItem::Checkpoint(c) => checkpoint_views.get(c.checkpoint_id.as_str()).copied(),
                _ => None,
            };
            let mut row = match cache.get(id) {
                Some(built) if built.is_of(item, message, checkpoint, expanded) => built.row.clone(),
                _ => {
                    let row = item_view(item, message.map(|m| &**m), checkpoint, expanded, &mut prefixes);
                    cache.insert(id.clone(), BuiltRow { item: Arc::clone(*item), message: message.cloned(), expanded, row: row.clone() });
                    row
                }
            };
            // A message's images, once their URLs came (they are no part of the row as kept).
            if let (TurnItem::UserMessage(_), Some(message)) = (&***item, message) {
                row.images = images(model, message);
            }
            let entry = item.message_id().map_or(id.as_str(), |m| m.as_str());
            row.find_matches = counts.get(entry).copied().unwrap_or(0);
            row.can_retry = !page.working && last_error == Some(id);
            if active_entry == Some(entry) {
                row.find_active = true;
                active_item = Some(row.id.clone());
            }
            row
        })
        .collect();
    drop(cache);
    drop(prefixes);
    group_steps(&mut page.items);
    // Turn times: on each finished turn's last row; and since when the
    // current one has been working.
    let ms = |at: Option<&Timestamp>| at.map_or(0, |at| crate::app::millis_from_iso(at.as_str()));
    for (idx, item) in shown.iter().enumerate() {
        let run_id = item.run_id();
        let last_of_run = shown.get(idx + 1).is_none_or(|next| next.run_id() != run_id);
        if !last_of_run {
            continue;
        }
        let Some(run) = run_id.and_then(|r| thread.runs.get(r)) else { continue };
        let (started, completed) = (ms(run.started_at.as_ref()), ms(run.completed_at.as_ref()));
        if started > 0 && !matches!(run.status, RunStatus::Running | RunStatus::Preparing | RunStatus::Starting | RunStatus::Waiting | RunStatus::Queued) {
            page.items[idx].run_started_ms = started;
            page.items[idx].run_duration_ms = (completed - started).max(0);
        }
    }
    if page.working {
        let under_way = thread.runs.iter().filter(|r| matches!(r.status, RunStatus::Running | RunStatus::Preparing | RunStatus::Starting));
        page.working_since_ms = under_way.map(|r| ms(r.started_at.as_ref())).max().unwrap_or(0);
    }
    for (row, item) in page.items.iter_mut().zip(&shown) {
        match &***item {
            // A question's row shows the answers given (on the request, not the item).
            TurnItem::UserInputRequest(question) => {
                if let Some(answers) = thread.runtime_requests.get(&question.request_id).and_then(|r| r.answers.as_ref()) {
                    row.text = answers.values().map(answer_text).filter(|a| !a.is_empty()).collect::<Vec<_>>().join(" · ");
                }
            }
            // A subagent that can be stopped, and what it is doing.
            TurnItem::Subagent(of) => {
                if let Some(sub) = thread.subagents.get(&of.subagent_id) {
                    let live = matches!(sub.status, SubagentStatus::Running | SubagentStatus::Pending | SubagentStatus::Waiting);
                    row.status = sub.status.as_str().to_owned();
                    row.progress = if live { sub.progress.clone().unwrap_or_default() } else { String::new() };
                    row.can_stop = live && sub.native_task_ref.as_ref().is_some_and(|task| task.native_id.is_some());
                    if let Some(r) = sub.result.as_deref().filter(|r| !r.trim().is_empty()) {
                        row.output = Some(tail(r).0.into());
                    }
                }
            }
            // A plan that can be carried out.
            TurnItem::ProposedPlan(_) | TurnItem::Other(_) if !row.plan_id.is_empty() => {
                let active = matches!(thread.plans.get(row.plan_id.as_str()), Some(PlanArtifact::ProposedPlan(plan)) if plan.status == PlanStatus::Active);
                row.can_implement = active && !page.working && !row.streaming;
            }
            _ => {}
        }
    }
    page.pending_requests = thread
        .runtime_requests
        .iter()
        .filter(|r| r.status == RuntimeRequestStatus::Pending)
        .map(|r| {
            let items = thread.turn_items.as_slice();
            let item = items.iter().find(|i| request_of(i) == Some(&r.id));
            // An edit waiting for approval: the change it would make, from
            // the provider's item the request belongs to (same native item).
            let files = item
                .filter(|it| it.native_item_ref().is_some())
                .map(|it| {
                    let edits: Vec<&Arc<TurnItem>> = items.iter().filter(|other| matches!(&***other, TurnItem::FileChange(_)) && other.native_item_ref() == it.native_item_ref()).collect();
                    let mut cache = model.approval_diffs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match cache.get(r.id.as_str()) {
                        // Worked out from these very edits: none has changed since.
                        Some((from, files)) if from.len() == edits.len() && from.iter().zip(&edits).all(|(a, b)| Arc::ptr_eq(a, b)) => files.clone(),
                        _ => {
                            let files: Vec<DiffFileView> = edits
                                .iter()
                                .flat_map(|edit| match &***edit {
                                    TurnItem::FileChange(change) => file_change_diffs(change),
                                    _ => vec![],
                                })
                                .collect();
                            if cache.len() >= 16 {
                                cache.clear(); // answered requests' diffs, now and then
                            }
                            cache.insert(r.id.as_str().to_owned(), (edits.into_iter().cloned().collect(), files.clone()));
                            files
                        }
                    }
                })
                .unwrap_or_default();
            let (prompt, asks) = match item.map(|i| &**i) {
                Some(TurnItem::ApprovalRequest(approval)) => (approval.prompt.as_deref(), vec![]),
                Some(TurnItem::UserInputRequest(question)) => (None, questions(question)),
                _ => (None, vec![]),
            };
            let asked = if r.kind == RuntimeRequestKind::UserInput { "The agent has a question" } else { "Approval required" };
            RequestView { id: r.id.as_str().to_owned(), kind: r.kind.as_str().to_owned(), prompt: prompt.unwrap_or(asked).to_owned(), detail: None, questions: asks, files }
        })
        .collect();
    let finished = thread.runs.iter().any(|r| match r.status {
        RunStatus::Completed | RunStatus::Failed | RunStatus::Interrupted => true,
        RunStatus::Cancelled => r.started_at.is_some(),
        _ => false,
    });
    page.can_fork = finished;
    page.can_merge_back = finished && !page.working && page.parent.as_ref().is_some_and(|p| p.relation == "fork" && model.thread_shell(&p.thread_id).is_some());
    page.context = context_usage(thread);
    page.background = background_label(thread);
    let queued = queued_runs(thread);
    let steerable = shell.is_some_and(|shell| steerable_run(model, shell).is_some());
    let count = queued.len();
    page.queue_held = queued.iter().any(|r| r.queue_held == Some(true));
    page.queued = queued
        .iter()
        .enumerate()
        .map(|(n, r)| {
            let text = thread.messages.get(&r.user_message_id).map(|m| m.text.clone()).unwrap_or_default();
            QueuedMessageView { run_id: r.id.as_str().to_owned(), text, can_steer: steerable, first: n == 0, last: n + 1 == count }
        })
        .collect();
    page.checkpoints = cps;
    if model.find.open {
        page.find = Some(FindView {
            query: model.find.query.clone(),
            total: find.map_or(0, |found| narrow(found.total_matches)),
            active: find.filter(|found| found.total_matches > 0).map_or(0, |found| narrow(found.active_index) + 1),
            active_item_id: active_item,
        });
    }
    Some(page)
}

/// The open thread's queued runs, in the order they run.
pub fn queued_order(model: &Model) -> Option<Vec<String>> {
    Some(queued_runs(model.thread.as_ref()?).iter().map(|r| r.id.as_str().to_owned()).collect())
}

/// The background work the provider still does after its reply.
fn background_label(thread: &Thread) -> String {
    let tasks: Vec<&PendingBackgroundTask> = thread.provider_threads.iter().flat_map(|t| t.pending_background_tasks.iter().flatten()).collect();
    let agents = tasks.iter().filter(|t| t.kind == Some(BackgroundTaskKind::Subagent)).count();
    match (agents, tasks.len() - agents) {
        (0, 0) => String::new(),
        (1, 0) => "Waiting for a background agent".into(),
        (n, 0) => format!("Waiting for {n} background agents"),
        (0, 1) => "Waiting for background work".into(),
        (a, w) => format!("Waiting for {} background tasks", a + w),
    }
}

/// The context window of the thread's provider conversation, as last reported.
fn context_usage(thread: &Thread) -> Option<ContextUsageView> {
    let reported = |t: &&ProviderThread| matches!(t.context_usage, Some(Some(_)));
    let active = thread.record.active_provider_thread_id.as_ref();
    let pt = active
        .and_then(|id| thread.provider_threads.get(id))
        .filter(reported)
        .or_else(|| thread.provider_threads.iter().filter(reported).max_by(|a, b| a.updated_at.cmp(&b.updated_at)))?;
    let usage = pt.context_usage.as_ref()?.as_ref()?;
    let used = Some(usage.used_tokens).filter(|n| *n > 0)?;
    let max = usage.max_tokens.unwrap_or(0);
    let percent = if max > 0 { ((used as f64 / max as f64) * 100.0).round().min(100.0) as u32 } else { 0 };
    let label = if max > 0 { format!("{} of {} tokens", token_label(used), token_label(max)) } else { format!("{} tokens", token_label(used)) };
    Some(ContextUsageView { used_tokens: used, max_tokens: max, percent, label, high: percent >= 80 })
}

/// `950`, `48k`, `1.2M`.
fn token_label(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", (n as f64 / 1000.0).round() as u64),
        _ => {
            let m = n as f64 / 1_000_000.0;
            if m >= 10.0 { format!("{}M", m.round() as u64) } else { format!("{:.1}M", m).replace(".0M", "M") }
        }
    }
}

fn runtime_modes() -> Vec<ChoiceView> {
    [("approval-required", "Ask for approval"), ("auto-accept-edits", "Auto-accept edits"), ("full-access", "Full access")]
        .into_iter()
        .map(|(value, label)| ChoiceView { value: value.into(), label: label.into() })
        .collect()
}

/// The most commands the `/` list shows.
const SLASH_LIMIT: usize = 8;

/// The provider's commands matching a draft that is `/` and one word: names
/// starting with the word first, then ones containing it.
fn slash_commands(model: &Model, instance_id: &str, draft: &str) -> Vec<SlashCommandView> {
    let Some(query) = draft.strip_prefix('/').filter(|q| !q.contains(char::is_whitespace)) else { return vec![] };
    let query = query.to_lowercase();
    let provider = model.providers.iter().find(|p| p.instance_id.as_str() == instance_id);
    let all: Vec<SlashCommandView> = provider
        .and_then(|p| p.slash_commands.as_ref())
        .into_iter()
        .flatten()
        .map(|c| SlashCommandView { name: c.name.clone(), description: c.description.clone().unwrap_or_default() })
        .collect();
    if all.iter().any(|c| c.name.to_lowercase() == query) {
        return vec![]; // typed in full
    }
    let (mut starts, contains): (Vec<_>, Vec<_>) = all.into_iter().filter(|c| c.name.to_lowercase().contains(&query)).partition(|c| c.name.to_lowercase().starts_with(&query));
    starts.extend(contains);
    starts.truncate(SLASH_LIMIT);
    starts
}

/// How well a model matches a picker search, lower being better, or `None`
/// when it doesn't: every word of the query has to be in the model's name,
/// slug or provider. The name matching the whole query ranks first (exactly,
/// at its start, at a word's start, anywhere), then by how well each word
/// matches the name.
pub(crate) fn model_match_score(query: &str, name: &str, slug: &str, provider: &str) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    let haystack = format!("{name}\n{}\n{}", slug.to_lowercase(), provider.to_lowercase());
    let words: Vec<&str> = query.split_whitespace().collect();
    if !words.iter().all(|w| haystack.contains(w)) {
        return None;
    }
    let in_name = |q: &str| -> u32 {
        if name == q {
            0
        } else if name.starts_with(q) {
            1
        } else if name.split(|c: char| !c.is_alphanumeric() && c != '.').any(|w| w.starts_with(q)) {
            2
        } else if name.contains(q) {
            3
        } else {
            4
        }
    };
    if name.contains(&query) {
        return Some(in_name(&query));
    }
    Some(4 + words.iter().map(|w| in_name(w)).sum::<u32>())
}

/// The upstream provider of an OpenCode or Pi model (`openrouter` in
/// `openrouter/anthropic/claude`), empty for other providers.
fn upstream_provider(driver: &str, slug: &str) -> String {
    if !matches!(driver, "opencode" | "pi") {
        return String::new();
    }
    slug.split_once('/').map(|(p, _)| p.to_owned()).unwrap_or_default()
}

/// The model picker: favorites first, then the other models by provider,
/// searched by `model_query`; and how many models it can show.
fn model_picker(model: &Model, selected: Option<&ModelOption>) -> (Vec<ModelRowView>, Vec<ModelGroupView>, u32) {
    let query = model.composer.model_query.trim();
    let settings = model.settings.as_ref();
    let favorites = update::model_favorites(settings);
    let is_selected = |o: &ModelOption| selected.is_some_and(|s| s.instance_id == o.instance_id && s.model == o.model);
    // Every model as a row, with its provider's name (for search).
    let mut rows: Vec<(ModelRowView, String)> = vec![];
    let mut hidden_by: HashMap<String, Vec<String>> = HashMap::new();
    for (m, o) in model_options(model) {
        let p = model.providers.iter().find(|p| p.instance_id.as_str() == o.instance_id);
        let hidden = hidden_by.entry(o.instance_id.clone()).or_insert_with(|| update::hidden_models(settings, &o.instance_id));
        let row = ModelRowView {
            name: if m.name.is_empty() { o.model.clone() } else { m.name.clone() },
            detail: upstream_provider(p.map_or("", |p| p.driver.as_str()), &o.model),
            favorite: favorites.iter().any(|(i, s)| *i == o.instance_id && *s == o.model),
            hidden: hidden.contains(&o.model),
            option: o,
        };
        rows.push((row, p.and_then(|p| p.display_name.clone()).unwrap_or_default()));
    }
    let count = rows.iter().filter(|(r, _)| !r.hidden).count() as u32;
    let score = |r: &ModelRowView, provider: &str| model_match_score(query, &r.name, &r.option.model, provider);
    // Keeps the rows matching the search, best first (ties keep their order).
    let search = |list: Vec<(ModelRowView, String)>| -> Vec<ModelRowView> {
        let mut scored: Vec<(u32, ModelRowView)> = list.into_iter().filter_map(|(r, p)| score(&r, &p).map(|n| (n, r))).collect();
        scored.sort_by_key(|(n, _)| *n);
        scored.into_iter().map(|(_, r)| r).collect()
    };

    let favorite_rows: Vec<(ModelRowView, String)> = favorites
        .iter()
        .filter_map(|(i, slug)| rows.iter().find(|(r, _)| r.option.instance_id == *i && r.option.model == *slug))
        .map(|(r, p)| {
            let detail = if r.detail.is_empty() { p.clone() } else { format!("{p} · {}", r.detail) };
            (ModelRowView { detail, ..r.clone() }, p.clone())
        })
        .collect();
    let favorite_models = search(favorite_rows);

    let mut groups: Vec<ModelGroupView> = vec![];
    let mut order: Vec<String> = vec![];
    for (r, _) in &rows {
        if !order.contains(&r.option.instance_id) {
            order.push(r.option.instance_id.clone());
        }
    }
    for instance in order {
        let p = model.providers.iter().find(|p| p.instance_id.as_str() == instance);
        let mut mine: Vec<(ModelRowView, String)> = rows.iter().filter(|(r, _)| r.option.instance_id == instance && !r.favorite).cloned().collect();
        let hidden_count = mine.iter().filter(|(r, _)| r.hidden).count() as u32;
        let custom: &[String] = update::model_preference(settings, &instance).and_then(|preference| preference.model_order.as_deref()).unwrap_or_default();
        if !custom.is_empty() {
            mine.sort_by_key(|(r, _)| custom.iter().position(|m| *m == r.option.model).unwrap_or(usize::MAX));
        } else if p.is_some_and(|p| matches!(p.driver.as_str(), "opencode" | "pi")) {
            // OpenCode and Pi list models in no particular order.
            mine.sort_by_cached_key(|(r, _)| (r.detail.clone(), r.name.to_lowercase()));
        }
        let models = if query.is_empty() {
            mine.into_iter().filter(|(r, _)| !r.hidden || model.composer.show_hidden_models || is_selected(&r.option)).map(|(r, _)| r).collect()
        } else {
            search(mine)
        };
        if models.is_empty() && (!query.is_empty() || hidden_count == 0) {
            continue;
        }
        let provider = p.and_then(|p| p.display_name.clone()).unwrap_or_default();
        let status = p.map(|p| p.status.as_str().to_owned()).unwrap_or_default();
        groups.push(ModelGroupView { instance_id: instance, provider, status, models, hidden_count });
    }
    (favorite_models, groups, count)
}

fn composer(model: &Model) -> ComposerView {
    let selected = current_model(model);
    let (favorite_models, groups, model_count) = model_picker(model, selected.as_ref());
    let efforts = selected
        .as_ref()
        .and_then(|m| effort_descriptor(model, m))
        .map(|d| d.options().iter().map(|o| ChoiceView { value: o.id.clone(), label: o.label.clone() }).collect())
        .unwrap_or_default();
    let blocked = selected.as_ref().map(|m| crate::update::provider_blocked(model, &m.instance_id)).unwrap_or_default();
    let draft = model.draft();
    let is_new = model.selected_thread.is_none();
    let working = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)).is_some_and(|t| matches!(shell_status(t), ShellStatus::Working | ShellStatus::Approval));
    let has_content = !draft.trim().is_empty() || !model.composer.attachments.is_empty();
    let slash = selected.as_ref().map(|m| slash_commands(model, &m.instance_id, &draft)).unwrap_or_default();
    ComposerView {
        draft,
        draft_revision: model.composer.draft_revision,
        placeholder: if is_new { "Ask anything, @ to mention files".into() } else if working { "Queue a follow-up…".into() } else { "Reply…".into() },
        attachments: model
            .composer
            .attachments
            .iter()
            .map(|a| AttachmentView { id: a.local_id.clone(), name: a.name.clone(), size_label: bytes_label(a.size_bytes as f64), uploading: a.uploading })
            .collect(),
        model_groups: groups,
        favorite_models,
        model_query: model.composer.model_query.clone(),
        model_count,
        show_hidden_models: model.composer.show_hidden_models,
        selected_effort: selected.as_ref().and_then(|m| current_effort(model, m)),
        selected_model: selected.clone(),
        efforts,
        runtime_modes: runtime_modes(),
        runtime_mode: composer_runtime_mode(model),
        interaction_mode: composer_interaction_mode(model),
        workspace_mode: if model.composer.workspace_mode.is_empty() { "local".into() } else { model.composer.workspace_mode.clone() },
        base_branch: model.composer.base_branch.clone().or_else(|| model.panel.git.local.as_ref().and_then(|local| local.ref_name.clone())),
        branches: model.composer.branches.clone(),
        is_new_thread: is_new,
        slash_commands: slash,
        pr_worktree: if is_new { model.composer.existing_worktree.as_ref().map(|w| w.2.clone()).unwrap_or_default() } else { String::new() },
        mention: model.composer.mention.as_ref().map(|m| MentionView {
            query: m.query.clone(),
            suggestions: m.results.iter().map(entry_view).collect(),
            highlighted: m.highlighted as u32,
        }),
        // Not connected but on the way back: what's sent waits in the outbox.
        can_send: can_queue(model)
            && has_content
            && !model.composer.sending
            && (!is_new || model.selected_project.is_some())
            && blocked.is_empty(),
        can_stop: working,
        can_steer: has_content
            && model.connection == Connection::Connected
            && model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)).is_some_and(|t| steerable_run(model, t).is_some()),
        shows_interaction_mode: selected
            .as_ref()
            .and_then(|m| model.providers.iter().find(|p| p.instance_id.as_str() == m.instance_id))
            .is_none_or(|p| p.show_interaction_mode_toggle != Some(false)),
        sending: model.composer.sending,
        send_hint: if !blocked.is_empty() {
            String::new()
        } else if model.connection == Connection::Connected {
            if model.composer.sending { "Starting the thread…".into() } else { String::new() }
        } else if can_queue(model) {
            "Not connected. What you send goes once the server is back.".into()
        } else {
            "Not connected to the server.".into()
        },
        queues_offline: model.connection != Connection::Connected && can_queue(model),
        blocked_reason: blocked,
    }
}

/// Connected, or reconnecting to a server this device is signed in to.
fn can_queue(model: &Model) -> bool {
    match model.connection {
        Connection::Connected => true,
        Connection::Reconnecting | Connection::Connecting => model.token.is_some() && !model.server_changed,
        _ => false,
    }
}

fn is_directory(e: &ProjectEntry) -> bool {
    e.kind == ProjectEntryKind::Directory
}

fn entry_view(e: &ProjectEntry) -> FileEntryView {
    let path = e.path.clone();
    let depth = path.matches('/').count() as u32;
    FileEntryView { name: path.rsplit('/').next().unwrap_or(&path).to_owned(), path, depth, directory: is_directory(e), expanded: false, ignored: e.ignored == Some(true) }
}

fn file_tree(model: &Model, dir: &str, depth: u32, out: &mut Vec<FileEntryView>) {
    let Some(children) = model.panel.files.children.get(dir) else { return };
    let mut sorted: Vec<&ProjectEntry> = children.iter().collect();
    sorted.sort_by(|a, b| is_directory(b).cmp(&is_directory(a)).then(a.path.to_lowercase().cmp(&b.path.to_lowercase())));
    for e in sorted {
        let mut v = entry_view(e);
        v.depth = depth;
        v.expanded = model.panel.files.expanded.contains(&v.path);
        let (path, expanded, is_dir) = (v.path.clone(), v.expanded, v.directory);
        out.push(v);
        if is_dir && expanded {
            file_tree(model, &path, depth + 1, out);
        }
    }
}

fn panel(model: &Model) -> PanelView {
    let p = &model.panel;
    let files = match &p.diff.files_override {
        Some(f) => f.clone(),
        None => p.diff.files().to_vec(),
    };
    let mut entries = vec![];
    file_tree(model, "", 0, &mut entries);
    let local = p.git.local.as_ref();
    let remote = p.git.remote.as_ref();
    let tree = local.map(|local| &local.working_tree);
    PanelView {
        open: p.open,
        tab: if p.tab.is_empty() { "diff".into() } else { p.tab.clone() },
        diff: DiffPanelView {
            sources: update::diff_sources(model).into_iter().map(|(value, label)| ChoiceView { value, label }).collect(),
            selected_source: p.diff.source.clone(),
            loading: p.diff.loading,
            error: p.diff.error.clone(),
            files,
            truncated: p.diff.truncated,
        },
        terminal: TerminalPanelView {
            tabs: p.terminals.iter().map(|t| TerminalTabView { id: t.id.clone(), label: t.label.clone(), running: t.running(), active: p.active_terminal.as_deref() == Some(t.id.as_str()) }).collect(),
            active: p.active_terminal.as_deref().and_then(|id| p.terminals.iter().find(|t| t.id == id)).map(|t| t.screen_view()),
        },
        files: FilesPanelView {
            root: p.files.root.clone(),
            entries,
            loading: p.files.loading.contains(""),
            query: p.files.query.clone(),
            results: p.files.results.iter().map(entry_view).collect(),
            preview: p.files.preview.as_ref().map(|pv| FilePreviewView { path: pv.path.clone(), spans: pv.spans.clone(), text: pv.text.clone(), truncated: pv.truncated, error: pv.error.clone(), loading: pv.loading }),
        },
        git: GitPanelView {
            loading: local.is_none(),
            is_repo: local.is_some_and(|local| local.is_repo),
            branch: local.and_then(|local| local.ref_name.clone()).filter(|name| !name.is_empty()),
            has_upstream: remote.is_some_and(|remote| remote.has_upstream),
            ahead: remote.map_or(0, |remote| narrow(remote.ahead_count)),
            behind: remote.map_or(0, |remote| narrow(remote.behind_count)),
            changed_files: tree.into_iter().flat_map(|tree| &tree.files).map(|f| ChangedFileView { path: f.path.clone(), insertions: narrow(f.insertions), deletions: narrow(f.deletions) }).collect(),
            insertions: tree.map_or(0, |tree| narrow(tree.insertions)),
            deletions: tree.map_or(0, |tree| narrow(tree.deletions)),
            pull_request: remote.and_then(|remote| remote.pr.as_ref()).map(|pr| PullRequestRowView {
                key: String::new(),
                repository: String::new(),
                number: narrow(pr.number),
                title: pr.title.clone(),
                state: pr.state.as_str().to_owned(),
                draft: pr.is_draft == Some(true),
                url: pr.url.clone(),
                head: pr.head_ref.clone(),
                base: pr.base_ref.clone(),
                ..Default::default()
            }),
            branches: model.composer.branches.clone(),
            busy: p.git.busy,
            progress: p.git.progress.clone(),
            error: p.git.error.clone(),
            has_remote: local.is_some_and(|local| local.has_primary_remote),
        },
    }
}

/// Palette entries for the current query: actions, threads, projects.
pub fn palette_items(model: &Model) -> Vec<PaletteItemView> {
    let Some(p) = &model.palette else { return vec![] };
    let q = p.query.trim().to_lowercase();
    let hit = |t: &str| q.is_empty() || t.to_lowercase().contains(&q);
    let mut items = vec![];
    let thread_selected = model.selected_thread.is_some();
    let actions = [
        ("new-thread", "New thread", "⌘N", true),
        ("toggle-panel", "Toggle side panel", "⌘J", true),
        ("diff", "Show diff", "", true),
        ("terminal", "Show terminal", "", thread_selected),
        ("files", "Show files", "", true),
        ("git", "Show git", "", true),
        ("find", "Find in thread", "⌘F", thread_selected),
        ("pin", "Pin or unpin thread", "", thread_selected),
        ("settle", "Settle thread", "", thread_selected),
        ("pull-requests", "Pull requests", "", true),
        ("usage", "Usage", "", true),
        ("scratch", "New thread without a project", "", true),
    ];
    for (id, title, shortcut, enabled) in actions {
        if enabled && hit(title) {
            items.push(PaletteItemView { id: format!("action:{id}"), kind: "action".into(), title: title.into(), subtitle: String::new(), shortcut: shortcut.into() });
        }
    }
    let mut threads: Vec<&ThreadShell> = model.threads.iter().filter(|t| t.deleted_at.is_none() && hit(&t.title)).collect();
    threads.sort_by_key(|t| std::cmp::Reverse(t.updated_at.as_str()));
    for t in threads.into_iter().take(20) {
        let project = model.project(t.project_id.as_str()).map(|p| p.title.clone()).unwrap_or_default();
        items.push(PaletteItemView { id: format!("thread:{}", t.id), kind: "thread".into(), title: t.title.clone(), subtitle: project, shortcut: String::new() });
    }
    for pr in model.projects.iter().filter(|p| hit(&p.title)).take(10) {
        items.push(PaletteItemView { id: format!("project:{}", pr.id), kind: "project".into(), title: format!("New thread in {}", pr.title), subtitle: pr.workspace_root.clone(), shortcut: String::new() });
    }
    items
}

fn field_bool(model: &Model, key: &str, label: &str, help: &str) -> SettingFieldView {
    SettingFieldView { key: key.into(), label: label.into(), help: help.into(), kind: "bool".into(), bool_value: settings::switch(model, key), ..Default::default() }
}

fn field_text(model: &Model, key: &str, label: &str, help: &str) -> SettingFieldView {
    SettingFieldView { key: key.into(), label: label.into(), help: help.into(), kind: "text".into(), text_value: settings::text(model, key), ..Default::default() }
}

fn field_choice(model: &Model, key: &str, label: &str, help: &str, choices: &[(&str, &str)]) -> SettingFieldView {
    SettingFieldView {
        key: key.into(),
        label: label.into(),
        help: help.into(),
        kind: "choice".into(),
        text_value: settings::choice(model, key),
        choices: choices.iter().map(|(v, l)| ChoiceView { value: (*v).into(), label: (*l).into() }).collect(),
        ..Default::default()
    }
}

fn setting_groups(model: &Model) -> Vec<SettingGroupView> {
    if model.settings.is_none() {
        return vec![];
    }
    vec![
        SettingGroupView {
            title: "New threads".into(),
            fields: vec![
                field_choice(model, "defaultRuntimeMode", "Permissions", "Default permissions for new threads.", &[("approval-required", "Ask for approval"), ("auto-accept-edits", "Auto-accept edits"), ("full-access", "Full access")]),
                // `responseStreamingMode` is left out: neither the server nor these
                // apps act on it yet, and a switch that does nothing misleads.
                field_bool(model, "newWorktreesStartFromOrigin", "Worktrees start from origin", "New worktrees branch from the remote's latest commit."),
                field_bool(model, "fastWorktreeCheckout", "Fast worktree checkout", "Reuse checked-out files when creating worktrees."),
                field_text(model, "worktreesDirectory", "Worktrees folder", "Where new worktrees are created. Empty uses the server's default."),
                field_choice(model, "branchNamingMode", "Branch names", "How branches for new worktrees are named.", &[("static", "Prefix and id"), ("semantic", "From the request"), ("custom", "Custom instructions")]),
                field_text(model, "branchNamePrefix", "Branch prefix", "Prefix for generated branch names."),
            ],
        },
        SettingGroupView {
            title: "Sidebar".into(),
            fields: vec![
                field_text(model, "sidebarAutoSettleAfterDays", "Settle after days", "Threads with no activity move to Settled after this many days."),
                field_bool(model, "sidebarAutoSettleOnMerge", "Settle when the pull request merges", ""),
                field_bool(model, "sidebarAutoSettleKeepPullRequests", "Keep threads with open pull requests", ""),
            ],
        },
        SettingGroupView {
            title: "Projects".into(),
            fields: vec![
                field_text(model, "addProjectBaseDirectory", "Add project starts in", "The folder the Add Project dialog opens."),
                field_bool(model, "defaultAutoPull", "Pull automatically", "Keep the default branch current when the checkout is clean."),
                field_bool(model, "ghqCloneLayout", "Clone into host folders", "Clones go to github.com/owner/repo under the folder above."),
            ],
        },
        SettingGroupView {
            title: "Agents".into(),
            fields: vec![
                field_bool(model, "keepAwakeWhileAgentsWork", "Keep the machine awake while agents work", ""),
                field_bool(model, "enableAgentBrowserAccess", "Agents can use the browser", ""),
                field_bool(model, "enableProviderUpdateChecks", "Check for provider updates", ""),
                field_bool(model, "continueThreadsAfterServerUpdate", "Continue threads after a server update", ""),
            ],
        },
        SettingGroupView {
            title: "Source control".into(),
            fields: vec![
                field_bool(model, "removeAgentCreditsOnMerge", "Remove agent credits when merging", "Strip co-author lines added by agents from merge commits."),
                field_bool(model, "reviewGuidesEnabled", "Review guides", "Generate a guide for reviewing pull requests."),
            ],
        },
    ]
}

pub fn pr_key(e: &PullRequestListEntry) -> String {
    format!("{}#{}#{}", e.project_id, e.repository, e.number)
}

fn settings(model: &Model) -> SettingsView {
    let pages = &model.pages;
    let access = pages.access.as_ref();
    let mut access_rows: Vec<AccessEntryView> = access
        .into_iter()
        .flat_map(|access| &access.client_sessions)
        .map(|c| AccessEntryView {
            id: c.session_id.clone(),
            kind: "session".into(),
            label: c.client.label.clone().filter(|label| !label.is_empty()).unwrap_or_else(|| if c.method == ServerAuthSessionMethod::BrowserSessionCookie { "Browser".into() } else { c.subject.clone() }),
            detail: {
                let os = c.client.os.as_deref().filter(|os| !os.is_empty()).map(|o| format!("{o} · ")).unwrap_or_default();
                format!("{os}Signed in {}", age_label(model.now_ms, c.issued_at.as_str()))
            },
            current: c.current,
            device_type: Some(c.client.device_type.as_str()).filter(|kind| !kind.is_empty()).unwrap_or(AuthClientDeviceType::Unknown.as_str()).to_owned(),
            connected: c.connected,
        })
        .collect();
    access_rows.extend(access.into_iter().flat_map(|access| &access.pairing_links).map(|l| AccessEntryView {
        id: l.id.clone(),
        kind: "pairing".into(),
        label: l.label.clone().filter(|label| !label.is_empty()).unwrap_or_else(|| "Pairing code".into()),
        detail: format!("Expires {}", l.expires_at),
        current: false,
        device_type: String::new(),
        connected: false,
    }));
    let remote = pages.remote.as_ref();
    let link = pages.remote_link.as_ref();
    let remote_view = RemoteAccessView {
        available: remote.is_some(),
        enabled: remote.is_some_and(|remote| remote.enabled),
        running: remote.is_some_and(|remote| remote.running),
        endpoint_id: remote.map(|remote| remote.endpoint_id.clone()).unwrap_or_default(),
        home_relay: remote.and_then(|remote| remote.home_relay.clone()).unwrap_or_default(),
        relays: remote.map(|remote| remote.relays.clone()).unwrap_or_default(),
        link: link.map(|link| link.link.clone()).unwrap_or_default(),
        code: link.map(|link| link.code.clone()).unwrap_or_default(),
        link_expires_at: link.map(|link| link.expires_at.as_str().to_owned()).unwrap_or_default(),
        busy: pages.remote_busy,
        error: pages.remote_error.clone().or_else(|| remote.and_then(|remote| remote.error.clone())),
        state: remote.map(|remote| remote.state.as_str().to_owned()).unwrap_or_default(),
        push_configured: remote.is_some_and(|remote| remote.push.configured),
    };
    SettingsView {
        groups: setting_groups(model),
        providers: model
            .providers
            .iter()
            .map(|p| ProviderView {
                instance_id: p.instance_id.as_str().to_owned(),
                name: p.display_name.clone().unwrap_or_default(),
                driver: p.driver.as_str().to_owned(),
                status: p.status.as_str().to_owned(),
                version: p.version.clone().unwrap_or_default(),
                auth: p.auth.label.as_deref().unwrap_or(p.auth.status.as_str()).to_owned(),
                message: p.message.clone().unwrap_or_default(),
                model_count: p.models.len() as u32,
                enabled: p.enabled,
            })
            .collect(),
        keybindings: model.keybindings.iter().map(keybinding_view).collect(),
        scheduled_tasks: pages
            .scheduled
            .iter()
            .map(|t| ScheduledTaskView {
                id: t.id.as_str().to_owned(),
                name: t.title.clone(),
                prompt: t.prompt.clone(),
                schedule: schedule_label(&t.schedule),
                enabled: t.enabled,
                next_run: t.next_run_at.as_ref().map(|n| n.as_str().replace('T', " ").chars().take(16).collect()).unwrap_or_else(|| "—".into()),
                last_run: match &t.last_run_status {
                    ScheduledTaskRunStatus::Never => "Never run".into(),
                    status => format!("{} {}", status.as_str(), age_label(model.now_ms, t.last_run_at.as_ref().map_or("", Timestamp::as_str))),
                },
                project_title: model.project(t.project_id.as_str()).map(|p| p.title.clone()).unwrap_or_default(),
            })
            .collect(),
        scheduled_time_zone: pages.scheduled_time_zone.clone(),
        archived: crate::sidebar::thread_rows(model, &model.archived),
        storage: StorageView {
            database_size: pages.database.as_ref().and_then(|db| db.database_bytes.as_f64()).map(bytes_label).unwrap_or_default(),
            reclaimable: pages.database.as_ref().and_then(|db| db.reclaimable_bytes.as_f64()).map(bytes_label).unwrap_or_default(),
            last_cleanup: match &pages.cleanup {
                Some(cleanup) => format!(
                    "{} removed, {} kept, {} freed, {}",
                    cleanup.counts.removed,
                    cleanup.counts.kept,
                    bytes_label(cleanup.bytes_freed.as_ref().and_then(serde_json::Number::as_f64).unwrap_or(0.0)),
                    age_label(model.now_ms, cleanup.finished_at.as_str())
                ),
                None => "Not run yet".into(),
            },
            busy: pages.storage_busy,
        },
        diagnostics: DiagnosticsView {
            cpu: pages.host.as_ref().and_then(|host| Some((host.cpu_utilization.as_ref()?.as_f64()?, host.cpu_count))).map(|(c, cores)| format!("{:.0}% of {cores} cores", c * 100.0)).unwrap_or_default(),
            memory: match &pages.host {
                Some(host) => format!("{} free of {}", bytes_label(host.available_memory_bytes as f64), bytes_label(host.total_memory_bytes as f64)),
                None => String::new(),
            },
            processes: pages
                .diagnostics
                .iter()
                .flat_map(|diagnostics| &diagnostics.processes)
                .map(|p| ProcessView {
                    pid: narrow(p.pid),
                    command: p.command.chars().take(160).collect(),
                    cpu: format!("{:.1}%", p.cpu_percent.as_f64().unwrap_or(0.0)),
                    memory: bytes_label(p.rss_bytes as f64),
                    depth: narrow(p.depth),
                })
                .collect(),
        },
        access: access_rows,
        remote: remote_view,
        projects: model
            .projects
            .iter()
            .map(|p| ProjectSettingsView {
                id: p.id.as_str().to_owned(),
                title: p.title.clone(),
                workspace_root: p.workspace_root.clone(),
                thread_count: model.threads.iter().filter(|t| t.project_id == p.id && t.deleted_at.is_none()).count() as u32,
            })
            .collect(),
    }
}

fn keybinding_view(k: &ResolvedKeybindingRule) -> KeybindingView {
    let sc = &k.shortcut;
    let mut parts = vec![];
    for (held, sym) in [(sc.mod_key, "⌘"), (sc.ctrl_key, "⌃"), (sc.alt_key, "⌥"), (sc.shift_key, "⇧"), (sc.meta_key, "⌘")] {
        if held && !parts.contains(&sym.to_owned()) {
            parts.push(sym.to_owned());
        }
    }
    parts.push(match sc.key.as_str() {
        " " => "Space".into(),
        "enter" => "↩".into(),
        "escape" => "Esc".into(),
        k if k.len() == 1 => k.to_uppercase(),
        k => k.to_owned(),
    });
    KeybindingView { command: k.command.as_str().to_owned(), shortcut: parts.join(""), condition: k.when_ast.as_ref().map(when_text).unwrap_or_default() }
}

fn when_text(n: &KeybindingWhenNode) -> String {
    match n {
        KeybindingWhenNode::Identifier { name } => name.clone(),
        KeybindingWhenNode::Not { node } => format!("!{}", when_text(node)),
        KeybindingWhenNode::And { left, right } => format!("{} && {}", when_text(left), when_text(right)),
        KeybindingWhenNode::Or { left, right } => format!("({} || {})", when_text(left), when_text(right)),
        KeybindingWhenNode::Other(_) => String::new(),
    }
}

fn schedule_label(s: &ScheduledTaskSchedule) -> String {
    match s {
        ScheduledTaskSchedule::Interval(interval) => {
            let mins = interval.every_ms / 60_000;
            match mins {
                m if m % 1440 == 0 && m > 0 => format!("Every {} day(s)", m / 1440),
                m if m % 60 == 0 && m > 0 => format!("Every {} hour(s)", m / 60),
                m => format!("Every {m} minute(s)"),
            }
        }
        ScheduledTaskSchedule::FixedTime(fixed) => format!("Daily at {}", fixed.time_of_day),
        ScheduledTaskSchedule::Once(once) => format!("Once at {}", once.run_at),
        ScheduledTaskSchedule::Webhook(_) => "On webhook".into(),
        ScheduledTaskSchedule::Other(_) => String::new(),
    }
}

fn pull_requests(model: &Model) -> PullRequestsView {
    let p = &model.pages;
    let d = p.pr_detail.as_ref();
    PullRequestsView {
        loading: p.prs_loading,
        error: p.prs_error.clone(),
        notice: p.prs_notice.clone(),
        busy: p.pr_busy.is_some(),
        items: p
            .prs
            .iter()
            .map(|e| PullRequestRowView {
                key: pr_key(e),
                repository: e.repository.clone(),
                number: narrow(e.number),
                title: e.title.clone(),
                author: e.author.as_ref().map(|author| author.login.clone()).unwrap_or_default(),
                state: e.state.as_str().to_owned(),
                draft: e.is_draft,
                url: e.url.clone(),
                head: e.head_branch.clone(),
                base: e.base_branch.clone(),
                age: age_label(model.now_ms, e.updated_at.as_str()),
                checks: e.checks_state.as_ref().map(|checks| checks.as_str().to_owned()).unwrap_or_default(),
            })
            .collect(),
        selected: p.pr_selected.as_ref().map(|key| {
            // What the list's row says of it, before the summary arrives.
            let listed = p.prs.iter().find(|e| pr_key(e) == *key);
            PullRequestDetailView {
                key: key.clone(),
                title: d.map(|d| d.title.clone()).unwrap_or_default(),
                // A summary carries no description.
                body: markdown::parse(""),
                url: d.map(|d| d.url.clone()).unwrap_or_default(),
                state: d.map(|d| d.state.as_str().to_owned()).unwrap_or_default(),
                additions: d.and_then(|d| d.additions).map_or(0, narrow),
                deletions: d.and_then(|d| d.deletions).map_or(0, narrow),
                changed_files: d.and_then(|d| d.changed_files).map_or(0, narrow),
                checks: p.pr_checks.iter().flat_map(|checks| &checks.checks).map(|c| CheckView { name: c.name.clone(), status: c.status.as_str().to_owned(), url: c.url.clone().unwrap_or_default() }).collect(),
                loading: d.is_none(),
                draft: d.is_some_and(|d| d.is_draft == Some(true)) || listed.is_some_and(|e| e.is_draft),
                number: listed.map_or(0, |e| narrow(e.number)),
                base: d.map(|d| d.base_branch.clone()).filter(|b| !b.is_empty()).or_else(|| listed.map(|e| e.base_branch.clone())).unwrap_or_default(),
                head: d.map(|d| d.head_branch.clone()).filter(|b| !b.is_empty()).or_else(|| listed.map(|e| e.head_branch.clone())).unwrap_or_default(),
            }
        }),
    }
}

fn usage(model: &Model) -> UsageView {
    let summary = model.pages.usage.as_ref();
    let mut total = 0u64;
    let rows: Vec<UsageRowView> = summary
        .into_iter()
        .flat_map(|summary| &summary.buckets)
        .map(|b| {
            let t = &b.totals;
            let sum = t.uncached_input_tokens + t.cached_input_tokens + t.cache_creation_tokens + t.output_tokens + t.reasoning_tokens;
            total += sum;
            UsageRowView {
                day: b.day.clone(),
                provider: b.provider.as_str().to_owned(),
                model: b.model.clone(),
                input: count_label((t.uncached_input_tokens + t.cache_creation_tokens) as i64),
                cached: count_label(t.cached_input_tokens as i64),
                output: count_label((t.output_tokens + t.reasoning_tokens) as i64),
                total: count_label(sum as i64),
            }
        })
        .collect();
    UsageView {
        loading: model.pages.usage_loading,
        range_label: summary.map(|summary| format!("{} – {}", summary.since_day, summary.until_day)).unwrap_or_default(),
        rows,
        total_tokens: count_label(total as i64),
        notice: if summary.is_some_and(|summary| summary.pricing.status == UsagePricingStatus::Unavailable) { "Costs are not shown: this server has no price table.".into() } else { String::new() },
    }
}

pub fn view(model: &Model) -> ViewModel {
    let env = model.environment.as_ref();
    let reconnect = if model.connection == Connection::Reconnecting && !model.in_flight { model.retry_in } else { 0 };
    let remote = model.active_environment().is_some_and(crate::model::Environment::is_remote);
    ViewModel {
        connection: ConnectionView {
            showing_saved: model.from_cache,
            state: match model.connection {
                Connection::Disconnected => "disconnected",
                Connection::Pairing => "pairing",
                Connection::Connecting => "connecting",
                Connection::Connected => "connected",
                Connection::Reconnecting => "reconnecting",
                Connection::Failed => "failed",
            }
            .into(),
            server_url: model.server_url.clone(),
            error: model.error.clone(),
            server_version: env.map(|env| env.server_version.clone()).unwrap_or_default(),
            environment_label: env.map(|env| env.label.clone()).unwrap_or_default(),
            platform: env.map(|env| format!("{} {}", env.platform.os.as_str(), env.platform.arch.as_str()).trim().to_owned()).unwrap_or_default(),
            reconnect_in_seconds: reconnect,
            environments: model
                .environments
                .iter()
                .map(|e| EnvironmentView {
                    id: e.id.clone(),
                    label: e.label.clone(),
                    detail: if e.is_remote() {
                        let key = crate::environments::endpoint_key(&e.ticket);
                        format!("Remote · {}", crate::environments::prefix(&key, 10))
                    } else {
                        e.url.clone()
                    },
                    remote: e.is_remote(),
                    active: model.active_env.as_deref() == Some(e.id.as_str()),
                    server_id: e.server_id.clone(),
                })
                .collect(),
            active_environment_id: model.active_env.clone().unwrap_or_default(),
            remote: model.active_environment().is_some_and(crate::model::Environment::is_remote),
            path: if model.tunnel.connected { model.tunnel.path.clone() } else { String::new() },
            rtt_ms: model.tunnel.rtt_ms,
            needs_pairing: model.active_env.is_some() && (model.token.is_none() || model.server_changed),
            server_changed: model.server_changed,
            hint: if model.connection != Connection::Reconnecting || model.reconnect_attempts < 6 {
                String::new()
            } else if remote {
                "Still trying. If this server was reinstalled or its remote access reset, its link changed: pair again with a new link.".into()
            } else {
                "Still trying. Is this device on the same network as the server, and is the server running? Away from that network, pair with a remote link instead.".into()
            },
            retry_gen: model.retry_gen,
            offline: model.offline,
            attempting: model.in_flight,
        },
        toast: model.toast.as_ref().map(|t| ToastView {
            id: t.id,
            kind: t.kind.clone(),
            message: t.message.clone(),
            action_label: t.action_label.clone(),
            queued: model.toast_queue.len() as u32,
        }),
        sidebar: sidebar(model),
        route: if model.selected_thread.is_some() && model.route == "thread" { "thread".into() } else if model.route.is_empty() { "empty".into() } else { model.route.clone() },
        thread: thread_page(model),
        composer: composer(model),
        panel: panel(model),
        palette: model.palette.as_ref().map(|p| PaletteView { query: p.query.clone(), items: palette_items(model), highlighted: p.highlighted as u32 }),
        settings: settings(model),
        pull_requests: pull_requests(model),
        usage: usage(model),
        agent_sessions: agent_sessions(model),
        folder_browser: folder_browser(model),
    }
}

/// The agent a scan found sessions of, as named to the user.
pub fn source_label(source: &str) -> &'static str {
    match source {
        s if s.starts_with("codex") => "Codex",
        s if s.starts_with("pi") => "Pi",
        _ => "Claude Code",
    }
}

fn agent_sessions(model: &Model) -> AgentSessionsView {
    let st = &model.sessions;
    let two_weeks_ago = crate::app::iso_from_millis(model.now_ms - 14 * 24 * 60 * 60 * 1000);
    let candidates: Vec<AgentSessionCandidateView> = st
        .candidates
        .iter()
        .map(|c| {
            let sources: Vec<&str> = c.sources.iter().map(|x| source_label(x.as_str())).collect();
            let count = narrow(c.thread_count);
            let last = c.last_active_at.as_ref().map_or("", Timestamp::as_str);
            let sessions = match count {
                0 => "nothing from the last 30 days".to_owned(),
                1 => "1 session".to_owned(),
                n => format!("{n} sessions"),
            };
            AgentSessionCandidateView {
                path: c.path.clone(),
                title: c.title.clone(),
                sources: sources.join(" · "),
                thread_count: count,
                detail: match age_label(model.now_ms, last).as_str() {
                    "now" => format!("{sessions} · active now"),
                    age => format!("{sessions} · {age} ago"),
                },
                already_imported: c.already_imported,
                recent: last >= two_weeks_ago.as_str() && count > 0,
            }
        })
        .collect();
    AgentSessionsView {
        scanned: st.scanned,
        scanning: st.scanning,
        importing: st.importing(),
        done: st.done,
        total: st.total,
        error: st.error.clone(),
        available: candidates.iter().filter(|c| !c.already_imported && c.thread_count > 0).count() as u32,
        candidates,
    }
}

fn folder_browser(model: &Model) -> FolderBrowserView {
    let b = &model.browser;
    FolderBrowserView {
        path: b.path.clone(),
        parent_path: b.parent.clone(),
        entries: b.entries.iter().map(|e| FolderEntryView { name: e.name.clone(), full_path: e.full_path.clone() }).collect(),
        loading: b.loading,
        error: b.error.clone(),
        is_project: model.projects.iter().any(|p| p.workspace_root.trim_end_matches('/') == b.path.trim_end_matches('/')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(monogram("my cool app"), "MC");
        assert_eq!(bytes_label(1536.0), "1.5 KB");
        assert_eq!(count_label(1234567), "1,234,567");
        let every_two_hours: ScheduledTaskSchedule = serde_json::from_value(serde_json::json!({ "type": "interval", "everyMs": 7_200_000 })).unwrap();
        assert_eq!(schedule_label(&every_two_hours), "Every 2 hour(s)");
    }

    #[test]
    fn model_search_ranks_the_name_first() {
        let score = |q: &str, name: &str, slug: &str| model_match_score(q, name, slug, "OpenCode");
        assert_eq!(score("", "Anything", "x"), Some(0));
        assert_eq!(score("gpt 5", "GPT 5", "openai/gpt-5"), Some(0), "exact");
        assert!(score("son", "Sonnet 4.5", "s") < score("son", "Claude Sonnet 4.5", "s"), "the name's start before a word's start");
        assert!(score("sonnet", "Claude Sonnet", "s") < score("onnet", "Claude Sonnet", "s"), "a word's start before inside a word");
        assert!(score("4.5 sonnet", "Claude Sonnet 4.5", "s").is_some(), "words in any order");
        assert!(score("openrouter sonnet", "Claude Sonnet", "openrouter/claude-sonnet").is_some(), "the slug counts");
        assert!(score("opencode haiku", "Haiku", "anthropic/haiku").is_some(), "the provider counts");
        assert!(score("sonnet", "Claude Sonnet", "s") < score("openrouter", "Claude Sonnet", "openrouter/claude-sonnet"), "a name match before a slug match");
        assert_eq!(score("gpt opus", "GPT 5", "openai/gpt-5"), None, "every word has to match");
        assert_eq!(score("SONNET", "claude sonnet", "s"), Some(2), "case doesn't matter");
    }
}
