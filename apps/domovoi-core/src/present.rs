//! The view model, derived from the model on every render.

#![expect(clippy::disallowed_types, reason = "fields the schema leaves open: a tool call's input and output, an answer to a question, an item of a type this build does not know")]

use crate::app::age_label;
use crate::model::{BuiltRow, Connection, Model};
use crate::replies::shell_status;
use crate::settings;
use crate::sidebar::{is_pinned, is_settled, relation, sidebar};
use crate::update::{self, composer_interaction_mode, composer_runtime_mode, current_effort, current_model, effort_descriptor, model_options, steerable_run};
use crate::view::*;
use crate::{diff, markdown, tools};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use domovoi_model::typed::{
    AuthClientDeviceType, BackgroundTaskKind, ChatAttachment, CheckpointFileSummary, CheckpointId, CheckpointStatus, ConversationMessage,
    FileChangeItem, KeybindingWhenNode, PendingBackgroundTask, PlanArtifact, PlanStatus, PlanStep, PlanStepStatus, ProjectEntry,
    ProjectEntryKind, ProviderThread, PullRequestListEntry, ResolvedKeybindingRule, Run, RunId, RunStatus, RuntimeRequestId,
    RuntimeRequestKind, RuntimeRequestStatus, ScheduledTaskRunStatus, ScheduledTaskSchedule, ServerAuthSessionMethod,
    ServerProviderUsageUnavailableReason, ServerProviderUsageWindowKind, ShellStatus, SubagentStatus, Thread, ThreadShell,
    Timestamp, TurnItem, TurnItemStatus, UsagePricingStatus, UsageSourceStatus, UserInputRequestItem,
};

/// A count the server sends, as the views carry counts.
fn narrow(n: u64) -> u32 {
    n as u32
}

pub const PROJECT_COLORS: [&str; 18] = ["gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan", "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink", "rose"];

pub fn project_identity(title: &str) -> (String, &'static str) {
    use unicode_normalization::UnicodeNormalization;
    let normalized: String = title.nfkc().collect();
    let name = normalized.trim();
    static WORDS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let words: Vec<&str> = WORDS.get_or_init(|| regex::Regex::new(r"[\p{L}\p{N}]+").expect("project word pattern")).find_iter(name).map(|m| m.as_str()).collect();
    let text = if let Some(word) = words.first() {
        let first = word.chars().next().expect("the project word pattern matches at least one character");
        let second = word.chars().skip(1).find(|c| c.is_numeric())
            .or_else(|| if words.len() > 1 { words.last()?.chars().next() } else { word.chars().last() }).unwrap_or(first);
        format!("{first}{second}").to_uppercase().chars().take(2).collect()
    } else { "PR".into() };
    let seed = if name.is_empty() { "project".into() } else { name.to_lowercase() };
    let index = seed.chars().fold(0, |index, c| (index * 31 + c as usize) % PROJECT_COLORS.len());
    (text, PROJECT_COLORS[index])
}

pub fn monogram(title: &str) -> String { project_identity(title).0 }

/// A Git LFS file's card: its object, whether the server has it, and what
/// that means for the preview.
fn lfs_view(preview: &crate::model::Preview, lfs: &domovoi_model::typed::ProjectFileLfs) -> crate::view::LfsView {
    use domovoi_model::typed::LfsObjectState as State;
    let (state_label, message) = match &lfs.state {
        State::Cached if preview.image_url.is_none() && preview.text.is_empty() => ("Stored locally", Some("Binary file stored in Git LFS.")),
        State::Cached => ("Stored locally", None),
        State::Missing => ("Not downloaded", Some("This file is kept in Git LFS, and its object isn't on the server yet.")),
        State::TooLarge => ("Too large to preview", Some("Its Git LFS object is over the 20 MB preview limit.")),
        State::Mismatch => ("Doesn't match its pointer", Some("The server's copy of this Git LFS object differs from its pointer.")),
        State::Unsupported => ("Uses Git LFS extensions", Some("The object is stored transformed by Git LFS extensions, so it can't be shown.")),
        State::Other(state) => (state.as_ref(), None),
    };
    crate::view::LfsView {
        oid: lfs.oid.clone(),
        oid_short: lfs.oid.chars().take(10).collect(),
        size_label: bytes_label(lfs.size as f64),
        state: lfs.state.as_str().to_owned(),
        state_label: state_label.to_owned(),
        message: preview.fetch_error.clone().or(message.map(str::to_owned)),
        can_fetch: lfs.state == State::Missing,
        fetching: preview.fetching,
    }
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
                files: files.iter().map(|f| ChangedFileView { path: f.path.clone(), insertions: narrow(f.additions), deletions: narrow(f.deletions) }).collect(),
                diff_source: format!("turn:{n}"),
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
pub(crate) fn tail(text: &str) -> (String, bool) {
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
            allow_custom_answer: q.allow_custom_answer.unwrap_or(true),
            required: q.required.unwrap_or(true),
            options: q.options.iter().map(|o| QuestionOptionView {
                label: o.label.clone(), value: o.value.clone().unwrap_or_else(|| o.label.clone()), description: o.description.clone(),
            }).collect(),
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
fn answer_text(a: &Value, question: &domovoi_model::typed::UserInputQuestion) -> String {
    let label = |value: &str| {
        question.options.iter().find(|option| option.value.as_deref() == Some(value))
            .map_or_else(|| value.to_owned(), |option| option.label.clone())
    };
    match a {
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(label).collect::<Vec<_>>().join(", "),
        Value::String(s) => label(s),
        _ => String::new(),
    }
}

/// A plan's row: its markdown.
fn plan_row(v: &mut ItemView, plan_id: &str, markdown: &str) {
    v.kind = "plan".into();
    v.title = "Plan".into();
    v.plan_id = plan_id.to_owned();
    v.text = markdown.to_owned();
    v.blocks = markdown::parse(&v.text).into();
}

fn todo_views(steps: &[PlanStep]) -> Arc<Vec<TodoView>> {
    steps.iter().map(|step| TodoView {
        text: step.text.clone(),
        status: match &step.status {
            PlanStepStatus::Completed => "completed".into(),
            PlanStepStatus::Running => "in_progress".into(),
            PlanStepStatus::Other(status) if &**status == "in_progress" => "in_progress".into(),
            PlanStepStatus::Pending | PlanStepStatus::Other(_) => "pending".into(),
        },
    }).collect::<Vec<_>>().into()
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
fn item_view(item: &TurnItem, message: Option<&ConversationMessage>, checkpoint: Option<&CheckpointView>, expanded: bool, cwd: &str, prefixes: &mut FxHashMap<String, markdown::Prefix>) -> ItemView {
    let mut v = ItemView { id: item.id().as_str().to_owned(), status: item.status().as_str().to_owned(), ..Default::default() };
    let completed = *item.status() == TurnItemStatus::Completed;
    let ctx = tools::Ctx { cwd, expanded, duration_ms: step_duration(item) };
    match item {
        TurnItem::UserMessage(i) => {
            v.kind = "user".into();
            v.issue_card = i.issue.as_ref().map(|card| IssueCardView {
                issue_label: format!("{} {}", card.display_key, card.title),
                issue_url: card.url.clone(),
            });
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
            tools::command(&mut v, i, ctx);
            v.collapsed = true;
        }
        TurnItem::FileChange(i) => {
            let path = file_change_path(i);
            let diff = file_change_diff(i);
            let created = i.changes.as_ref().and_then(|c| c.first()).is_some_and(|c| matches!(c.operation.as_str(), "add" | "create" | "added"));
            let failed = *item.status() == TurnItemStatus::Failed;
            let error = i.error.as_deref().filter(|_| failed);
            tools::file_change_look(&mut v, &path, diff.as_ref(), created, error, *item.status() == TurnItemStatus::Running, ctx);
            v.diff = diff.map(Arc::new);
            v.collapsed = completed;
            if let Some(changes) = i.changes.as_ref().filter(|changes| !changes.is_empty()) {
                v.artifact_paths.extend(changes.iter().filter(|c| !matches!(c.operation.as_str(), "delete" | "remove"))
                    .map(|c| &c.path).filter(|path| crate::artifacts::artifact_kind_for_path(path).is_some()).cloned());
            } else if crate::artifacts::artifact_kind_for_path(&i.file_name).is_some() {
                v.artifact_paths.push(i.file_name.clone());
            }
        }
        TurnItem::TodoList(i) => {
            v.kind = "todo".into();
            v.title = i.explanation.as_deref().filter(|e| !e.trim().is_empty()).unwrap_or("To-dos").to_owned();
            v.todos = todo_views(&i.steps);
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
        TurnItem::DynamicTool(i)
            if matches!(
                i.tool_name.as_deref(),
                // The tool as Domovoi's server names it, and as it did while its tools were `t3_…` of a server `t3`.
                Some(
                    "Artifact" | "mcp__domovoi__artifact_create" | "mcp__domovoi__artifact_update" | "artifact_create" | "artifact_update"
                        | "mcp__t3__t3_artifact_create" | "mcp__t3__t3_artifact_update" | "t3_artifact_create" | "t3_artifact_update"
                )
            ) && !matches!(
                i.input.as_ref().and_then(|v| v.get("action")).and_then(Value::as_str),
                Some("list" | "delete" | "read")
            ) =>
        {
            v.kind = "artifact".into();
            v.collapsed = false;
            v.streaming = *item.status() == TurnItemStatus::Running
                || i.input.as_ref().and_then(|v| v.get("_streaming")).and_then(Value::as_bool) == Some(true);
            let meta = dynamic_tool_artifact_meta(i);
            v.title = meta.title.clone();
            v.artifact_title = meta.title;
            v.artifact_kind = meta.kind;
            v.artifact_hosted_url = meta.hosted_url;
        }
        TurnItem::DynamicTool(i) if matches!(i.tool_name.as_deref(), Some("ArtifactComments" | "ArtifactData")) => {
            tools::dynamic(&mut v, i, ctx);
            let action = i.input.as_ref().and_then(|v| v.get("action").or_else(|| v.get("db_op"))).and_then(Value::as_str).unwrap_or("update");
            let label = if i.tool_name.as_deref() == Some("ArtifactComments") { "Artifact comments" } else { "Artifact data" };
            v.title = format!("{label} · {action}");
            v.collapsed = true;
        }
        TurnItem::DynamicTool(i) => {
            tools::dynamic(&mut v, i, ctx);
            v.collapsed = true;
        }
        TurnItem::WebSearch(i) => {
            tools::web_search(&mut v, i, ctx);
            v.collapsed = true;
        }
        TurnItem::FileSearch(i) => {
            tools::file_search(&mut v, i, ctx);
            v.collapsed = true;
        }
        TurnItem::UserInputRequest(i) => {
            v.kind = "question".into();
            v.title = i.questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n");
        }
        TurnItem::ApprovalRequest(i) => {
            v.kind = "approval".into();
            v.title = i.prompt.as_deref().unwrap_or("Approval requested").to_owned();
            v.text = i.request_kind.as_str().to_owned();
        }
        TurnItem::ProposedPlan(i) => {
            v.streaming = i.streaming;
            plan_row(&mut v, i.plan_id.as_str(), &i.markdown);
        }
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
                "mcp_tool_call" => {
                    tools::mcp_call(&mut v, text("toolName").or(item.title()).unwrap_or("tool"), field("input"), field("output"), item.status(), ctx);
                    v.collapsed = true;
                }
                "plan" => plan_row(&mut v, text("planId").unwrap_or_default(), text("markdown").or(text("text")).unwrap_or_default()),
                _ => notice_row(&mut v, item, text("text").unwrap_or_default()),
            }
        }
        // A context divider: what compacting freed, and its summary to read.
        TurnItem::Compaction(i) => {
            v.kind = "notice".into();
            v.title = match (i.before_token_count, i.after_token_count) {
                (Some(before), Some(after)) if before > 0 => format!("Context compacted · {} → {} tokens", token_label(before), token_label(after)),
                (Some(before), _) if before > 0 => format!("Context compacted · was {} tokens", token_label(before)),
                _ if completed => "Context compacted".into(),
                _ => "Compacting context…".into(),
            };
            v.text = i.summary.clone().unwrap_or_default();
            v.collapsed = true;
        }
        TurnItem::Handoff(i) => {
            v.kind = "notice".into();
            v.title = match i.to_model.as_deref().filter(|m| !m.is_empty()) {
                Some(model) => format!("Continued with {model}"),
                None => format!("Continued with {}", i.to_provider_instance_id.as_str()),
            };
            v.text = i.summary.clone().unwrap_or_default();
            v.collapsed = true;
        }
        // The card to answer it is `ThreadPageView::secret_requests`; the row says how it went.
        TurnItem::SecretRequest(i) => {
            use domovoi_model::typed::SecretRequestStatus as Asked;
            v.kind = "notice".into();
            v.title = match &i.secret_status {
                Asked::Pending | Asked::Other(_) => format!("The agent asks for a secret: {}", i.label),
                Asked::Saved => format!("Secret saved: {}", i.label),
                Asked::Declined => format!("Secret declined: {}", i.label),
                Asked::Cancelled => format!("Secret no longer needed: {}", i.label),
            };
            v.text = i.reason.clone();
        }
        TurnItem::LinkRequest(i) => {
            v.kind = "notice".into();
            v.title = format!("The agent asks to link {}", i.label.as_deref().filter(|l| !l.is_empty()).or(i.url.as_deref()).unwrap_or("another machine"));
            v.text = i.reason.clone();
        }
        TurnItem::Notification(_)
        | TurnItem::RunInterruptRequest(_)
        | TurnItem::RunInterruptResult(_)
        | TurnItem::SystemNotice(_)
        | TurnItem::Fork(_)
        | TurnItem::ThreadCreated(_) => notice_row(&mut v, item, ""),
    }
    v
}

/// The thread's secret requests that wait for an answer, oldest first.
fn secret_requests(model: &Model, thread: &domovoi_model::typed::Thread) -> Vec<SecretRequestView> {
    thread
        .visible_turn_item_refs()
        .into_iter()
        .filter_map(|item| match item {
            TurnItem::SecretRequest(asked) if asked.secret_status == domovoi_model::typed::SecretRequestStatus::Pending => Some((item.id(), asked)),
            _ => None,
        })
        .map(|(id, asked)| {
            let file = asked.delivery.as_ref().is_some_and(|d| *d == domovoi_model::typed::SecretDelivery::File);
            SecretRequestView {
                item_id: id.as_str().to_owned(),
                label: asked.label.clone(),
                reason: asked.reason.clone(),
                url: asked.url.clone().filter(|url| url.starts_with("https://") || url.starts_with("http://")),
                placeholder: asked.placeholder.clone().unwrap_or_default(),
                delivery: asked.delivery.as_ref().map(|d| d.as_str().to_owned()).unwrap_or_default(),
                hint: if file {
                    "Stored on the server; the agent reads it from a private file, gone after 24 hours.".into()
                } else {
                    "Stored on the server; the agent gets a reference to it, never the value.".into()
                },
                sending: model.secrets_answering.contains(id.as_str()),
            }
        })
        .collect()
}

/// The pull requests (without the stack's dismissed ones) and issues linked to a thread.
fn work_links(shell: &ThreadShell) -> Vec<WorkLinkView> {
    use domovoi_model::typed::PullRequestLinkSource;
    let pull_requests = shell.pull_requests.iter().flatten().filter(|link| link.source != PullRequestLinkSource::StackDismissed).map(|link| {
        let snapshot = link.snapshot.as_ref();
        WorkLinkView {
            kind: "pull_request".into(),
            label: format!("#{}", link.number),
            title: snapshot.map(|s| s.title.clone()).unwrap_or_default(),
            url: link.url.clone(),
            state: snapshot.map(|s| if s.is_draft && s.state.as_str() == "open" { "draft" } else { s.state.as_str() }).unwrap_or_default().to_owned(),
        }
    });
    let issues = shell.issues.iter().flatten().map(|issue| WorkLinkView {
        kind: "issue".into(),
        label: if issue.display_key.is_empty() { issue.key.key.clone() } else { issue.display_key.clone() },
        title: issue.title.clone(),
        url: issue.url.clone(),
        state: String::new(),
    });
    pull_requests.chain(issues).collect()
}

/// How long a finished step took (0: unknown, or still under way).
fn step_duration(item: &TurnItem) -> i64 {
    match (item.started_at(), item.completed_at()) {
        (Some(start), Some(end)) => (crate::app::millis_from_iso(end.as_str()) - crate::app::millis_from_iso(start.as_str())).max(0),
        _ => 0,
    }
}

/// Thinking has a disclosure only when there is something to read.
fn has_readable_content(item: &TurnItem) -> bool {
    !matches!(item, TurnItem::Reasoning(i) if i.text.trim().is_empty())
}

/// A row of the open thread built again from its item, when the item's text
/// grew and nothing else changed (`Changes::row`): `old` is the row as last
/// built. `None` when the item is gone or no longer visible.
pub(crate) fn regrown_row(model: &Model, item_id: &str, old: &ItemView) -> Option<ItemView> {
    let thread = model.held_thread(model.selected_thread.as_deref()?)?;
    let item = thread.turn_items.get(item_id)?;
    if !has_readable_content(item) {
        return None;
    }
    let message = item.message_id().and_then(|id| thread.messages.get(id));
    let mut prefixes = model.markdown_prefixes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // The rows whose text grows (a reply, thinking, a command) show no checkpoint.
    let cwd = model.selected_thread.as_deref().and_then(|tid| model.thread_cwd(tid)).unwrap_or_default();
    let mut row = item_view(item, message, None, model.expanded.contains(item_id), &cwd, &mut prefixes);
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
        while i < items.len() && is_step(&items[i].kind) {
            i += 1;
        }
        if i - start < 2 {
            continue;
        }
        let summary = group_summary(&items[start..i]);
        let group = items[start].id.clone();
        for row in &mut items[start..i] {
            row.group.clone_from(&group);
        }
        items[start].group_summary = summary;
    }
}

/// What a group of steps did, most telling first: "Edited 2 files (+40 −12),
/// ran 5 commands, read 14 files and 3 other steps · 1 failed"; while one of
/// them runs, that one ("Reading present.rs…"); of thinking alone "Thought it over".
fn group_summary(rows: &[ItemView]) -> String {
    if let Some(live) = rows.iter().rev().find(|r| r.status == "running" && !r.verb.is_empty()) {
        let line = if live.title.is_empty() { live.verb.clone() } else { format!("{} {}", live.verb, live.title) };
        return format!("{}…", domovoi_model::typed::first_line_clipped(&line, 120).unwrap_or(line));
    }
    let distinct = |wanted: &dyn Fn(&ItemView) -> bool| -> usize {
        let mut seen: Vec<&str> = rows.iter().filter(|r| wanted(r)).map(|r| r.title.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    let count = |wanted: &dyn Fn(&ItemView) -> bool| rows.iter().filter(|r| wanted(r)).count();
    let edits = distinct(&|r| r.kind == "file_change");
    let (added, removed) = rows.iter().filter_map(|r| r.diff.as_deref()).fold((0, 0), |(a, d), diff| (a + diff.additions, d + diff.deletions));
    let edited = match (edits, added + removed) {
        (0, _) => String::new(),
        (n, 0) => format!("edited {}", plural(n, "file", "files")),
        (n, _) => format!("edited {} (+{added} −{removed})", plural(n, "file", "files")),
    };
    let searches = count(&|r| r.icon == "search");
    let searched = match searches {
        1 => rows.iter().find(|r| r.icon == "search").map(|r| format!("searched for {}", domovoi_model::typed::clip_chars(&r.title, 40))).unwrap_or_default(),
        n => counted(n, "searched", "time", "times"),
    };
    let parts: [(usize, String); 9] = [
        (edits, edited),
        (count(&|r| r.icon == "command"), counted(count(&|r| r.icon == "command"), "ran", "command", "commands")),
        (count(&|r| r.icon == "read"), counted(distinct(&|r| r.icon == "read"), "read", "file", "files")),
        (count(&|r| r.icon == "image"), counted(distinct(&|r| r.icon == "image"), "viewed", "image", "images")),
        (searches, searched),
        (count(&|r| r.icon == "list"), counted(count(&|r| r.icon == "list"), "listed", "folder", "folders")),
        (count(&|r| r.icon == "web"), counted(count(&|r| r.icon == "web"), "searched the web", "time", "times")),
        (count(&|r| r.icon == "fetch"), counted(count(&|r| r.icon == "fetch"), "fetched", "page", "pages")),
        (count(&|r| r.kind != "reasoning" && !matches!(r.icon.as_str(), "command" | "read" | "image" | "search" | "list" | "web" | "fetch" | "edit")), String::new()),
    ];
    let mut shown: Vec<&str> = vec![];
    let mut others = 0;
    for (n, text) in &parts {
        if *n == 0 {
            continue;
        }
        if shown.len() < 3 && !text.is_empty() {
            shown.push(text);
        } else {
            others += n;
        }
    }
    let mut summary = match (shown.len(), others) {
        (0, 0) => "thought it over".to_owned(),
        (0, n) => counted(n, "used", "tool", "tools"),
        (_, 0) => sentence(&shown),
        (_, n) => format!("{} and {}", shown.join(", "), plural(n, "other step", "other steps")),
    };
    let failed = rows.iter().filter(|r| r.badge_tone == "failed" || r.status == "failed").count();
    if failed > 0 {
        summary.push_str(&format!(" · {failed} failed"));
    }
    if let Some(first) = summary.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    summary
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn counted(n: usize, did: &str, one: &str, many: &str) -> String {
    format!("{did} {}", plural(n, one, many))
}

/// "a, b and c".
fn sentence(parts: &[&str]) -> String {
    match parts {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The images a step looked at or made, whose URLs have come.
fn step_images(model: &Model, item: &TurnItem, cwd: &str) -> Vec<ImageView> {
    let id = item.id().as_str();
    tools::image_sources(item, cwd)
        .into_iter()
        .filter_map(|source| {
            let url = model.asset_urls.get(&source.key(id)).filter(|u| !u.is_empty() && !u.contains("/api/assets/none/"))?;
            Some(ImageView { name: source.name(), url: format!("{}{url}", model.server_url) })
        })
        .collect()
}

/// The most images a folded group shows.
const GROUP_IMAGES: usize = 6;

/// A group's first row carries the images its steps viewed, so a folded
/// group still shows them.
fn gather_group_images(items: &mut [ItemView]) {
    let mut i = 0;
    while i < items.len() {
        if items[i].group.is_empty() || items[i].group_summary.is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        let group = items[start].group.clone();
        let mut gathered: Vec<ImageView> = vec![];
        while i < items.len() && items[i].group == group {
            for image in &items[i].images {
                if gathered.len() < GROUP_IMAGES && !gathered.iter().any(|g| g.url == image.url) {
                    gathered.push(image.clone());
                }
            }
            i += 1;
        }
        items[start].group_images = gathered;
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

fn working_activity<'a>(
    run_id: &RunId,
    items: impl DoubleEndedIterator<Item = (Option<&'a RunId>, &'a ItemView)> + Clone,
) -> Option<WorkingActivityView> {
    let mut current = items.rev().filter_map(|(id, row)| (id == Some(run_id)).then_some(row));
    let live_step = |row: &ItemView| is_step(&row.kind) && row.status == "running";
    let kind = current.clone().find(|row| live_step(row) || row.kind == "assistant" && row.streaming).map(|row| row.kind.clone());
    let step_item_id = current.clone().find(|row| live_step(row))
        .or_else(|| current.find(|row| is_step(&row.kind)))
        .map(|row| row.id.clone());
    (kind.is_some() || step_item_id.is_some()).then_some(WorkingActivityView { kind, step_item_id })
}

fn thread_page(model: &Model) -> Option<(ThreadPageView, Vec<crate::artifacts::CollectedArtifact>)> {
    let (mut page, collected) = thread_page_body(model)?;
    let shell = model.thread_shell(&page.id);
    page.can_create_pull_request = crate::pull_request_sheet::available(model);
    page.pull_request_sheet = crate::pull_request_sheet::view(model, &page.id);
    page.pull_request_status = shell.and_then(crate::pull_request_sheet::status);
    page.usage = session_usage(model, &page);
    Some((page, collected))
}

fn thread_page_body(model: &Model) -> Option<(ThreadPageView, Vec<crate::artifacts::CollectedArtifact>)> {
    let tid = model.selected_thread.as_deref()?;
    let shell = model.thread_shell(tid);
    let status = shell.map_or(ShellStatus::Idle, shell_status);
    let project_title = shell.and_then(|shell| model.project(shell.project_id.as_str())).map(|p| p.title.clone()).unwrap_or_default();
    let model_label = current_model(model).map(|m| m.label).unwrap_or_default();
    // Being started: shown from what was sent until the server's copy is.
    let launching = model.launching_shown(tid);
    let project_title = if project_title.is_empty() {
        launching.and_then(|l| model.project(&l.project_id)).map(|p| p.title.clone()).unwrap_or_default()
    } else {
        project_title
    };
    let mut page = ThreadPageView {
        id: tid.to_owned(),
        title: shell.map(|shell| shell.title.clone()).or_else(|| launching.map(|l| l.title.clone())).unwrap_or_default(),
        project_id: shell.map(|shell| shell.project_id.as_str().to_owned()).or_else(|| launching.map(|l| l.project_id.clone())).unwrap_or_default(),
        project_title,
        branch: shell.and_then(|shell| shell.branch.clone()).filter(|branch| !branch.is_empty()),
        worktree_path: shell.and_then(|shell| shell.worktree_path.clone()).filter(|path| !path.is_empty()),
        cwd: model.thread_cwd(tid).unwrap_or_default(),
        model_label,
        provider_instance_id: shell.map(|shell| shell.provider_instance_id.as_str().to_owned()).unwrap_or_default(),
        model_instance_id: shell.map(|shell| shell.model_selection.instance_id.as_str().to_owned()).unwrap_or_default(),
        model: shell.map(|shell| shell.model_selection.model.clone()).unwrap_or_default(),
        runtime_mode: composer_runtime_mode(model),
        interaction_mode: composer_interaction_mode(model),
        status: status.as_str().into(),
        working: launching.is_some() || matches!(status, ShellStatus::Working | ShellStatus::Approval),
        // Nothing to wait for while it is being started: its message is shown.
        // Records held for another thread are never shown as this one's.
        loading: launching.is_none() && (model.held_thread(tid).is_none() || model.thread_loading),
        has_earlier: model.history_before.is_some(),
        loading_earlier: model.history_loading,
        pinned: shell.is_some_and(is_pinned),
        settled: shell.is_some_and(is_settled),
        archived: shell.is_some_and(|shell| shell.archived_at.is_some()),
        ..Default::default()
    };
    if let Some(shell) = shell
        && shell.last_error_class.clone().flatten() == Some(domovoi_model::typed::ProviderFailureClass::UsageLimit)
        && !page.working
    {
        let reset = shell.usage_limit_reset_at.clone().flatten();
        page.limit_resets = reset.as_ref().map_or_else(|| "later".to_owned(), |r| crate::app::until_label(model.shown_ms, r.as_str()));
        page.limit_plan = match shell.limit_recovery.clone().flatten() {
            Some(r) if r.snooze == Some(true) && !r.auto_resume => "snooze".into(),
            Some(r) if r.auto_resume => "resume".into(),
            _ => String::new(),
        };
    }
    page.pending_sends = model
        .composer
        .outbox
        .iter()
        .filter(|o| o.draft_key == tid && !o.text.is_empty())
        // Already in the thread (its echo came before the answer).
        .filter(|o| !o.message_id.as_deref().is_some_and(|m| model.held_thread(tid).is_some_and(|t| t.messages.contains(m))))
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
    if let Some(l) = launching {
        let state = if model.connection == Connection::Connected { "sending" } else { "waiting" };
        page.pending_sends.insert(0, PendingSendView { id: l.message_id.clone(), text: l.text.clone(), state: state.into() });
        if model.connection == Connection::Connected && status != ShellStatus::Approval {
            page.working_activity = Some(WorkingActivityView { kind: Some("starting".into()), step_item_id: None });
        }
    }
    page.links = shell.map(work_links).unwrap_or_default();
    page.parent = shell.and_then(|shell| Some((shell, shell.lineage.parent_thread_id.as_ref()?))).map(|(shell, parent)| ThreadLinkView {
        thread_id: parent.as_str().to_owned(),
        title: model.thread_shell(parent.as_str()).map(|t| t.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Earlier thread".into()),
        relation: relation(shell).to_owned(),
    });
    let Some(thread) = model.held_thread(tid) else { return Some((page, Vec::new())) };
    let cps = checkpoints(thread);
    let checkpoint_views: FxHashMap<&str, &CheckpointView> = cps.iter().map(|c| (c.id.as_str(), c)).collect();
    let find = model.find.result.as_ref();
    let counts: HashMap<&str, u32> = find.and_then(|found| found.navigation.as_ref()).into_iter().flatten().map(|n| (n.entry_id.as_str(), narrow(n.count))).collect();
    let active_entry = find.and_then(|found| found.active_match.as_ref()).map(|active| active.entry_id.as_str());
    let mut active_item = None;
    let visible: Vec<_> = shown_items(thread).into_iter().filter(|item| has_readable_content(item)).collect();
    let runs_with = |wanted: fn(&TurnItem) -> bool| -> FxHashSet<&RunId> { visible.iter().filter(|i| wanted(i)).filter_map(|i| i.run_id()).collect() };
    // A run that failed and changed nothing gets no "No file changes" card.
    let failed_runs = runs_with(|i| matches!(i, TurnItem::Error(_)));
    // A turn whose to-dos came as a list doesn't show the tool call too.
    let todo_runs = runs_with(|i| matches!(i, TurnItem::TodoList(_)));
    // A question asked as a tool call is shown by its question's card.
    let question_runs = runs_with(|i| matches!(i, TurnItem::UserInputRequest(_)));
    let message_question_calls: FxHashSet<_> = visible.iter().filter_map(|item| {
        let TurnItem::UserInputRequest(question) = &***item else { return None };
        question.response_mode.as_ref()?;
        Some((question.run_id.as_ref()?, question.native_item_ref.as_ref()?.native_id.as_deref()?))
    }).collect();
    let in_runs = |item: &TurnItem, runs: &FxHashSet<&RunId>| item.run_id().is_some_and(|r| runs.contains(r));
    let shown: Vec<&Arc<TurnItem>> = visible
        .into_iter()
        .filter(|item| match &***item {
            TurnItem::Checkpoint(c) => !(c.files.is_empty() && in_runs(item, &failed_runs)),
            TurnItem::DynamicTool(tool) => match tool.tool_name.as_deref() {
                Some("TodoWrite") => !in_runs(item, &todo_runs),
                Some("AskUserQuestion") => !in_runs(item, &question_runs),
                Some("ask_question") => !tool.run_id.as_ref().zip(tool.native_item_ref.as_ref().and_then(|native| native.native_id.as_deref()))
                    .is_some_and(|call| message_question_calls.contains(&call)),
                // The plan it proposed is its own row.
                Some("ExitPlanMode") => false,
                _ => true,
            },
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
                    let row = item_view(item, message.map(|m| &**m), checkpoint, expanded, &page.cwd, &mut prefixes);
                    cache.insert(id.clone(), BuiltRow { item: Arc::clone(*item), message: message.cloned(), expanded, row: row.clone() });
                    row
                }
            };
            // A message's images, once their URLs came (they are no part of the row as kept).
            if let (TurnItem::UserMessage(_), Some(message)) = (&***item, message) {
                row.images = images(model, message);
            } else {
                row.images = step_images(model, item, &page.cwd);
                if row.icon == "image" && row.images.is_empty() && row.badge.is_empty()
                    && tools::image_sources(item, &page.cwd).iter().any(|source| {
                        model.asset_urls.get(&source.key(id.as_str())).is_some_and(|url| url.starts_with("/api/assets/none/"))
                    })
                {
                    row.badge = "Image no longer available".into();
                }
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
    gather_group_images(&mut page.items);
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
        // A finished turn's last reply can start a fork from there.
        if run.status == RunStatus::Completed
            && let Some(reply) = (0..=idx).rev().take_while(|&i| shown[i].run_id() == run_id).find(|&i| page.items[i].kind == "assistant")
        {
            page.items[reply].can_fork_here = true;
        }
    }
    if page.working && status != ShellStatus::Approval {
        let run = match shell.and_then(|shell| shell.active_run_id.as_ref()) {
            Some(id) => thread.runs.get(id),
            None => thread.runs.iter().rev().filter(|run| run.status == RunStatus::Waiting).max_by_key(|run| run.ordinal),
        };
        if let Some(run) = run.filter(|run| matches!(run.status, RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting)) {
            page.working_since_ms = ms(run.started_at.as_ref());
            page.working_activity = if matches!(run.status, RunStatus::Preparing | RunStatus::Starting) {
                Some(WorkingActivityView { kind: Some("starting".into()), step_item_id: None })
            } else {
                working_activity(&run.id, shown.iter().map(|item| item.run_id()).zip(&page.items))
            };
        }
    }
    for (row, item) in page.items.iter_mut().zip(&shown) {
        match &***item {
            // A question's row shows the answers given (on the request, not the item).
            TurnItem::UserInputRequest(question) => {
                if let Some(answers) = thread.runtime_requests.get(&question.request_id).and_then(|r| r.answers.as_ref()) {
                    row.text = question.questions.iter().filter_map(|question| answers.get(&question.id).map(|answer| answer_text(answer, question)))
                        .filter(|answer| !answer.is_empty()).collect::<Vec<_>>().join(" · ");
                }
            }
            // A subagent that can be stopped, and what it is doing.
            TurnItem::Subagent(of) => {
                if let Some(sub) = thread.subagents.get(&of.subagent_id) {
                    let live = matches!(sub.status, SubagentStatus::Running | SubagentStatus::Pending | SubagentStatus::Waiting);
                    let stoppable = !matches!(sub.driver.as_str(), "antigravity" | "acp");
                    row.status = sub.status.as_str().to_owned();
                    row.progress = if live { sub.progress.clone().unwrap_or_default() } else { String::new() };
                    row.can_stop = live && stoppable && sub.native_task_ref.as_ref().is_some_and(|task| task.native_id.is_some());
                    if let Some(r) = sub.result.as_deref().filter(|r| !r.trim().is_empty()) {
                        row.output = Some(tail(r).0.into());
                    }
                }
            }
            // A plan that can be carried out, or one a later plan replaced.
            TurnItem::ProposedPlan(_) | TurnItem::Other(_) if !row.plan_id.is_empty() => {
                let status = match thread.plans.get(row.plan_id.as_str()) {
                    Some(PlanArtifact::ProposedPlan(plan)) => Some(&plan.status),
                    _ => None,
                };
                row.can_implement = status == Some(&PlanStatus::Active) && !page.working && !row.streaming;
                row.superseded = status == Some(&PlanStatus::Superseded);
            }
            // A to-do list says how far along it is when folded.
            TurnItem::TodoList(_) | TurnItem::DynamicTool(_) if row.kind == "todo" && !row.todos.is_empty() => {
                let done = row.todos.iter().filter(|t| t.status == "completed").count();
                let current = row.todos.iter().find(|t| t.status == "in_progress").or_else(|| row.todos.iter().find(|t| t.status != "completed"));
                row.progress = match current {
                    Some(step) => format!("{done}/{} · {}", row.todos.len(), step.text),
                    None => format!("{done}/{} done", row.todos.len()),
                };
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
            let for_thread = r.native_request_ref.as_ref().is_some_and(|native| matches!(native.driver.as_str(), "claudeAgent" | "antigravity"));
            let session_label = if for_thread { "Allow for Thread" } else { "Allow for Session" }.to_owned();
            RequestView { id: r.id.as_str().to_owned(), kind: r.kind.as_str().to_owned(), prompt: prompt.unwrap_or(asked).to_owned(), detail: None, questions: asks, files, session_label }
        })
        .collect();
    page.secret_requests = secret_requests(model, thread);
    let finished = thread.runs.iter().any(|r| match r.status {
        RunStatus::Completed | RunStatus::Failed | RunStatus::Interrupted => true,
        RunStatus::Cancelled => r.started_at.is_some(),
        _ => false,
    });
    page.can_fork = finished;
    page.can_merge_back = finished && !page.working && page.parent.as_ref().is_some_and(|p| p.relation == "fork" && model.thread_shell(&p.thread_id).is_some());
    page.waiting_children = model
        .threads
        .iter()
        .filter(|t| t.lineage.parent_thread_id.as_ref().is_some_and(|p| p.as_str() == tid) && t.deleted_at.is_none() && t.archived_at.is_none())
        .filter(|t| shell_status(t) == ShellStatus::Approval)
        .map(|t| ThreadLinkView {
            thread_id: t.id.as_str().to_owned(),
            title: if t.title.is_empty() { "Subagent".into() } else { t.title.clone() },
            relation: relation(t).to_owned(),
        })
        .collect();
    let active_run = shell.and_then(|shell| shell.active_run_id.as_ref()).or_else(|| {
        thread.runs.iter().filter(|r| matches!(r.status, RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting))
            .max_by_key(|r| r.ordinal).map(|r| &r.id)
    });
    (page.plan, page.tasks) = work_progress(thread, &shown, &page.items, page.working, active_run);
    page.context = context_usage(thread);
    if let Some(context) = page.context.as_mut() {
        context.can_compact = !page.working && provider_has_command(model, &page.provider_instance_id, "compact");
    }
    page.background = background_label(thread);
    page.background_tasks = background_tasks(thread);
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
    let collected = collect_and_stamp_artifacts(thread, &mut page.items, &page.cwd);
    page.artifact_count = collected.len() as u32;
    Some((page, collected))
}

struct ToolArtifactMeta {
    explicit_id: Option<String>,
    title: String,
    kind: String,
    language: String,
    content: Option<String>,
    path: Option<String>,
    hosted_url: Option<String>,
    summary: String,
}

fn dynamic_tool_artifact_meta(i: &domovoi_model::typed::DynamicToolItem) -> ToolArtifactMeta {
    let input = i.input.as_ref();
    let output_val = i.output.as_ref();
    let parsed_output = output_val
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let out_obj = output_val.filter(|v| v.is_object()).or(parsed_output.as_ref());
    let str_in = |k: &str| input.and_then(|v| v.get(k)).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let str_out = |k: &str| out_obj.and_then(|v| v.get(k)).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);

    let output_text = output_val.and_then(Value::as_str).unwrap_or_default();
    let hosted_url = str_out("url")
        .or_else(|| str_out("hostedUrl"))
        .or_else(|| crate::artifacts::extract_claude_url(output_text))
        .or_else(|| str_in("url"));
    let content = input.and_then(|v| v.get("content")).and_then(Value::as_str).map(str::to_owned);
    let path = str_out("sourcePath").or_else(|| str_in("path"));
    let explicit_id = str_out("artifactId")
        .or_else(|| str_out("id"))
        .or_else(|| str_in("id"))
        .or_else(|| hosted_url.as_deref().and_then(|u| u.rsplit('/').next()).filter(|s| !s.is_empty()).map(|s| format!("claude:{s}")));
    let raw_kind = str_out("kind")
        .or_else(|| str_in("kind"))
        .or_else(|| str_in("type"))
        .or_else(|| path.as_deref().and_then(crate::artifacts::artifact_kind_for_path).map(str::to_owned))
        .unwrap_or_else(|| if content.as_deref().is_some_and(|c| !c.is_empty()) { "html".into() } else if hosted_url.is_some() { "hosted".into() } else { "html".into() });
    let kind = crate::artifacts::normalize_kind(&raw_kind).to_owned();
    let language = str_out("language").or_else(|| str_in("language")).unwrap_or_else(|| kind.clone());
    let title = str_out("title")
        .or_else(|| str_in("title"))
        .unwrap_or_else(|| crate::artifacts::extract_title(&kind, content.as_deref().unwrap_or_default(), path.as_deref()));
    let summary = str_in("summary").or_else(|| str_in("description")).unwrap_or_default();

    ToolArtifactMeta { explicit_id, title, kind, language, content, path, hosted_url, summary }
}

/// Collects all artifacts from the thread (explicit MCP/Claude tools, inline
/// HTML/SVG/React/Mermaid blocks, and previewable workspace files), grouping
/// versions and stamping artifact metadata onto `items` and their blocks.
fn collect_and_stamp_artifacts(thread: &Thread, items: &mut [ItemView], cwd: &str) -> Vec<crate::artifacts::CollectedArtifact> {
    let raw_by_id: FxHashMap<&str, &TurnItem> = thread.turn_items.iter().map(|i| (i.id().as_str(), i)).collect();
    let mut collector = crate::artifacts::Collector::default();

    for row in items.iter_mut() {
        let raw = raw_by_id.get(row.id.as_str()).copied();
        if row.kind == "artifact"
            && let Some(TurnItem::DynamicTool(tool)) = raw
        {
            let meta = dynamic_tool_artifact_meta(tool);
            let normalized_path = meta
                .path
                .as_deref()
                .and_then(|p| crate::artifacts::workspace_artifact_path(cwd, p));
            let (id, version) = collector.push(crate::artifacts::EntryInput {
                explicit_id: meta.explicit_id,
                item_id: row.id.clone(),
                title: meta.title.clone(),
                kind: meta.kind.clone(),
                language: meta.language,
                source: meta.content,
                path: normalized_path,
                hosted_url: meta.hosted_url.clone(),
                summary: meta.summary,
                streaming: row.streaming,
            });
            row.artifact_id = Some(id);
            row.artifact_title = meta.title;
            row.artifact_kind = meta.kind;
            row.artifact_version = version;
            row.artifact_hosted_url = meta.hosted_url;
            continue;
        }

        if row.kind == "assistant" && !row.streaming {
            let mut block_stamps: Vec<(usize, String, String, String, u32)> = Vec::new();
            let mut linked_paths: Vec<String> = Vec::new();
            for (idx, block) in row.blocks.iter().enumerate() {
                let block_kind = if block.kind == "html_document" {
                    Some("html")
                } else if block.kind == "svg_document" {
                    Some("svg")
                } else if block.kind == "code" && block.fence_closed {
                    crate::artifacts::artifact_kind_for_language(&block.language)
                } else {
                    None
                };
                if let Some(kind) = block_kind {
                    let title = crate::artifacts::extract_title(kind, &block.text, None);
                    let lang = if block.language.is_empty() { kind.to_owned() } else { block.language.clone() };
                    let (id, version) = collector.push(crate::artifacts::EntryInput {
                        explicit_id: None,
                        item_id: format!("{}:{idx}", row.id),
                        title: title.clone(),
                        kind: kind.to_owned(),
                        language: lang,
                        source: Some(block.text.clone()),
                        path: None,
                        hosted_url: None,
                        summary: String::new(),
                        streaming: false,
                    });
                    block_stamps.push((idx, id, title, kind.to_owned(), version));
                }
                for run in block.runs.iter().chain(block.cell_runs.iter().flatten()) {
                    if let Some(link) = &run.workspace_link {
                        linked_paths.push(link.clone());
                    }
                }
            }
            if !block_stamps.is_empty() {
                let blocks = Arc::make_mut(&mut row.blocks);
                for (idx, id, title, kind, version) in block_stamps {
                    if let Some(b) = blocks.get_mut(idx) {
                        b.artifact_id = Some(id);
                        b.artifact_title = title;
                        b.artifact_kind = kind;
                        b.artifact_version = version;
                    }
                }
            }
            for candidate in linked_paths {
                if let Some(path) = crate::artifacts::workspace_artifact_path(cwd, &candidate)
                    && let Some(kind) = crate::artifacts::artifact_kind_for_path(&path)
                {
                    let title = crate::artifacts::extract_title(kind, "", Some(&path));
                    collector.push(crate::artifacts::EntryInput {
                        explicit_id: None,
                        item_id: row.id.clone(),
                        title,
                        kind: kind.to_owned(),
                        language: crate::highlight::language_for_path(&path),
                        source: None,
                        path: Some(path),
                        hosted_url: None,
                        summary: String::new(),
                        streaming: false,
                    });
                }
            }
        }

        if !row.artifact_paths.is_empty() && !row.streaming {
            let mut first_stamp: Option<(String, String, String, u32)> = None;
            for candidate in &row.artifact_paths {
                if let Some(path) = crate::artifacts::workspace_artifact_path(cwd, candidate)
                    && let Some(kind) = crate::artifacts::artifact_kind_for_path(&path)
                {
                    let title = crate::artifacts::extract_title(kind, "", Some(&path));
                    let (id, version) = collector.push(crate::artifacts::EntryInput {
                        explicit_id: None,
                        item_id: row.id.clone(),
                        title: title.clone(),
                        kind: kind.to_owned(),
                        language: crate::highlight::language_for_path(&path),
                        source: None,
                        path: Some(path),
                        hosted_url: None,
                        summary: String::new(),
                        streaming: false,
                    });
                    if first_stamp.is_none() {
                        first_stamp = Some((id, title, kind.to_owned(), version));
                    }
                }
            }
            if let Some((id, title, kind, version)) = first_stamp {
                row.artifact_id = Some(id);
                row.artifact_title = title;
                row.artifact_kind = kind;
                row.artifact_version = version;
            }
        }
    }

    collector.finish()
}

/// Collects the active thread's artifacts for update-time actions (`OpenArtifact`,
/// `SelectArtifactVersion`, `AskAboutArtifact`).
pub fn collect_artifacts_for_model(model: &Model) -> Vec<crate::artifacts::CollectedArtifact> {
    thread_page(model).map(|(_, collected)| collected).unwrap_or_default()
}

/// Select from both collections. Older Claude runs can have a plan artifact
/// whose node points at the tool row that replaced the plan row.
fn work_progress(
    thread: &Thread, shown: &[&Arc<TurnItem>], rows: &[ItemView], working: bool, active_run: Option<&RunId>,
) -> (Option<PlanProgressView>, Option<TaskProgressView>) {
    let run = |id: Option<&RunId>| id.and_then(|id| thread.runs.get(id));
    let rank = |id: Option<&RunId>, ordinal: u64| (run(id).map_or(0, |r| r.ordinal), ordinal);
    let plan_nodes: FxHashSet<_> = thread.plans.iter().map(|plan| match plan {
        PlanArtifact::ProposedPlan(p) => &p.node_id,
        PlanArtifact::TodoList(t) => &t.node_id,
    }).collect();
    let mut by_node = FxHashMap::default();
    if !plan_nodes.is_empty() {
        by_node.extend(shown.iter().zip(rows).filter_map(|(item, row)| {
            let node = item.node_id().filter(|node| plan_nodes.contains(node))?;
            Some((node, (*item, row)))
        }));
    }
    let row_rank = |item: &TurnItem| rank(item.run_id(), item.ordinal());
    let plan_row = shown.iter().zip(rows).rev().find(|(_, row)| row.kind == "plan" && !row.text.trim().is_empty());
    let task_row = shown.iter().zip(rows).rev().find(|(item, row)| {
        row.kind == "todo" && !matches!(&****item, TurnItem::DynamicTool(t) if t.status == TurnItemStatus::Failed)
    });
    let artifact_rank = |id: Option<&RunId>, node: &domovoi_model::typed::NodeId| {
        rank(id, by_node.get(node).map_or(0, |(item, _)| item.ordinal()))
    };
    let is_rolled_back = |id| run(id).is_some_and(|run| run.status == RunStatus::RolledBack);
    let stored_plan = thread.plans.iter().filter_map(|plan| match plan {
        PlanArtifact::ProposedPlan(p) if !p.markdown.trim().is_empty() && !is_rolled_back(p.run_id.as_ref()) => Some(p),
        _ => None,
    }).max_by_key(|p| artifact_rank(p.run_id.as_ref(), &p.node_id));
    let plan = if let Some(p) = stored_plan.filter(|p| plan_row.is_none_or(|(item, row)| {
        row.plan_id == p.id.as_str() || artifact_rank(p.run_id.as_ref(), &p.node_id) >= row_rank(item)
    })) {
        let row = plan_row.map(|(_, row)| row).filter(|row| row.plan_id == p.id.as_str());
        let streaming = row.is_some_and(|r| r.streaming);
        Some(PlanProgressView {
            plan_id: p.id.as_str().to_owned(),
            label: plan_label(Some(&p.status), streaming).to_owned(),
            can_implement: p.status == PlanStatus::Active && !streaming && !working,
            blocks: row.filter(|r| r.text == p.markdown).map(|r| r.blocks.clone()).unwrap_or_else(|| markdown::parse(&p.markdown).into()),
        })
    } else {
        plan_row.map(|(_, row)| {
            let status = match thread.plans.get(row.plan_id.as_str()) {
                Some(PlanArtifact::ProposedPlan(p)) => Some(&p.status),
                _ => None,
            };
            PlanProgressView {
                plan_id: row.plan_id.clone(), label: plan_label(status, row.streaming).to_owned(),
                blocks: row.blocks.clone(), can_implement: row.can_implement,
            }
        })
    };

    let stored_tasks = thread.plans.iter().filter_map(|plan| match plan {
        PlanArtifact::TodoList(t) if !is_rolled_back(t.run_id.as_ref()) => Some(t),
        _ => None,
    }).max_by_key(|t| artifact_rank(t.run_id.as_ref(), &t.node_id));
    let checklist = if let Some(t) = stored_tasks.filter(|t| task_row.is_none_or(|(item, _)| {
        artifact_rank(t.run_id.as_ref(), &t.node_id) >= row_rank(item)
    })) {
        Some((t.run_id.as_ref(), t.explanation.clone(), by_node.get(&t.node_id)
            .filter(|(_, row)| row.kind == "todo").map(|(_, row)| row.todos.clone()).unwrap_or_else(|| todo_views(&t.steps))))
    } else {
        task_row.map(|(item, row)| {
            let explanation = match &***item {
                TurnItem::TodoList(t) => t.explanation.clone(),
                _ => None,
            };
            (item.run_id(), explanation, row.todos.clone())
        })
    };
    let tasks = checklist.filter(|(_, _, todos)| !todos.is_empty()).map(|(run_id, explanation, todos)| {
        let done = todos.iter().filter(|t| t.status == "completed").count() as u32;
        let current = todos.iter().find(|t| t.status == "in_progress").or_else(|| todos.iter().find(|t| t.status != "completed"));
        let total = todos.len() as u32;
        let current = current.map(|step| step.text.clone()).unwrap_or_default();
        let previous_run = working && (active_run.is_none() || active_run != run_id);
        let label = if previous_run { format!("Tasks {done}/{total} · Previous run") }
            else if current.is_empty() { format!("Tasks {done}/{total}") }
            else { format!("Tasks {done}/{total} · {current}") };
        TaskProgressView { done, total, current, label, previous_run, explanation, todos }
    });
    (plan, tasks)
}

fn plan_label(status: Option<&PlanStatus>, streaming: bool) -> &'static str {
    match status {
        Some(PlanStatus::Draft) => "Plan draft",
        _ if streaming => "Plan draft",
        Some(PlanStatus::Active) => "Plan ready",
        Some(PlanStatus::Completed) => "Plan accepted",
        Some(PlanStatus::Superseded) => "Plan superseded",
        _ => "Plan",
    }
}

/// The open thread's queued runs, in the order they run.
pub fn queued_order(model: &Model) -> Option<Vec<String>> {
    Some(queued_runs(model.held_thread(model.selected_thread.as_deref()?)?).iter().map(|r| r.id.as_str().to_owned()).collect())
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

fn background_tasks(thread: &Thread) -> Vec<BackgroundTaskView> {
    let nonblank = |text: Option<&String>| text.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let mut seen_ids = FxHashSet::default();
    let mut seen_children = FxHashSet::default();
    let mut tasks = Vec::new();
    for provider in thread.provider_threads.iter() {
        for task in provider.pending_background_tasks.iter().flatten() {
            let agent = if task.kind == Some(BackgroundTaskKind::Subagent) {
                let by_child = task.child_thread_id.as_ref().and_then(|id| {
                    thread.subagents.iter().find(|agent| agent.child_thread_id.as_ref() == Some(id))
                });
                by_child.or_else(|| {
                    let native = task.native_item_id.as_ref().unwrap_or(&task.task_id);
                    thread.subagents.iter().find(|agent| {
                        agent.provider_thread_id.as_ref() == Some(&provider.id)
                            && agent.native_task_ref.as_ref().and_then(|r| r.native_id.as_ref()) == Some(native)
                    })
                })
            } else {
                None
            };
            let status = match agent.map(|agent| &agent.status) {
                Some(SubagentStatus::Completed | SubagentStatus::Failed | SubagentStatus::Cancelled | SubagentStatus::Interrupted) => continue,
                Some(SubagentStatus::Idle | SubagentStatus::Pending) => "pending",
                Some(SubagentStatus::Waiting) => "waiting",
                _ => "running",
            };
            let child = task.child_thread_id.as_ref().or_else(|| agent.and_then(|agent| agent.child_thread_id.as_ref()));
            // The provider may replace a launch tool ID with a task ID once it acknowledges it.
            let id = if let Some(native) = &task.native_item_id {
                format!("{}:item:{native}", provider.id.as_str())
            } else {
                format!("{}:task:{}", provider.id.as_str(), task.task_id)
            };
            if !seen_ids.insert(id.clone()) || child.is_some_and(|child| !seen_children.insert(child)) {
                continue;
            }
            let (kind, fallback) = match task.kind {
                Some(BackgroundTaskKind::Subagent) => ("subagent", "Background agent"),
                Some(BackgroundTaskKind::Command) => ("command", "Background command"),
                Some(BackgroundTaskKind::Monitor) => ("monitor", "Background monitor"),
                _ => ("task", "Background task"),
            };
            let label = nonblank(agent.and_then(|agent| agent.title.as_ref()))
                .or_else(|| nonblank(task.description.as_ref()))
                .or_else(|| nonblank(task.command.as_ref()))
                .unwrap_or_else(|| fallback.into());
            tasks.push(BackgroundTaskView {
                id, kind: kind.into(), label, status: status.into(),
                progress: nonblank(agent.and_then(|agent| agent.progress.as_ref())),
                child_thread_id: child.map(|id| id.as_str().to_owned()),
            });
        }
    }
    tasks
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
    let remaining_label = if max > 0 {
        if used >= max { "Full".into() } else { format!("{} left", token_label(max - used)) }
    } else {
        String::new()
    };
    let cache_hit_percent = match (usage.input_tokens, usage.cached_input_tokens) {
        (Some(inp), Some(cached)) if inp > 0 && cached > 0 => ((cached.min(inp) as f64 / inp as f64) * 100.0).round().min(100.0) as u32,
        _ => 0,
    };
    let cache_hit_label = if cache_hit_percent > 0 { format!("{cache_hit_percent}%") } else { String::new() };
    let compact_at_percent = match (usage.auto_compact_threshold, max) {
        (Some(at), max) if max > 0 && at > 0 => ((at as f64 / max as f64) * 100.0).round().min(100.0) as u32,
        _ => 0,
    };
    let breakdown = [
        ("Input", usage.input_tokens),
        ("Cached", usage.cached_input_tokens),
        ("Output", usage.output_tokens),
        ("Reasoning", usage.reasoning_output_tokens),
    ]
    .into_iter()
    .filter_map(|(label, n)| n.filter(|n| *n > 0).map(|n| StatLineView { label: label.into(), value: token_label(n) }))
    .collect();
    let cost = usage.cost.as_ref().and_then(|c| Some((c.amount.as_f64()?, c.currency.as_str()))).filter(|(amount, _)| *amount > 0.0).map(|(amount, currency)| money_label(amount, currency)).unwrap_or_default();
    Some(ContextUsageView {
        used_tokens: used,
        max_tokens: max,
        percent,
        label,
        remaining_label,
        cache_hit_percent,
        cache_hit_label,
        high: percent >= 80,
        critical: percent >= 90,
        compact_at_percent,
        breakdown,
        cost,
        can_compact: false,
    })
}

/// The provider lists this slash command (`compact`, without its `/`).
pub(crate) fn provider_has_command(model: &Model, instance_id: &str, name: &str) -> bool {
    model
        .providers
        .iter()
        .find(|p| p.instance_id.as_str() == instance_id)
        .and_then(|p| p.slash_commands.as_ref())
        .is_some_and(|commands| commands.iter().any(|c| c.name.trim_start_matches('/') == name))
}

/// `$1.24`, `€0.08`, `12.00 PLN`.
pub(crate) fn money_label(amount: f64, currency: &str) -> String {
    match currency.to_ascii_uppercase().as_str() {
        "USD" | "" => format!("${amount:.2}"),
        "EUR" => format!("€{amount:.2}"),
        "GBP" => format!("£{amount:.2}"),
        other => format!("{amount:.2} {other}"),
    }
}

/// The usage windows of a provider's account in a stable semantic order:
/// Session, Weekly (overall before model-scoped), Monthly, then other.
fn limit_windows(now_ms: i64, limits: Option<&domovoi_model::typed::ServerProviderUsageLimits>) -> Vec<LimitWindowView> {
    let mut ordered: Vec<_> = limits.into_iter().flat_map(|l| l.windows.iter()).collect();
    fn window_sort_key(w: &domovoi_model::typed::ServerProviderUsageWindow) -> (u8, bool, &str) {
        let kind_rank: u8 = match w.kind {
            ServerProviderUsageWindowKind::Session => 0,
            ServerProviderUsageWindowKind::Weekly => 1,
            ServerProviderUsageWindowKind::Monthly => 2,
            _ => 3,
        };
        let scoped = w.model_ids.as_ref().is_some_and(|m| !m.is_empty()) || w.label.contains('·');
        (kind_rank, scoped, w.label.as_str())
    }
    ordered.sort_by(|a, b| window_sort_key(a).cmp(&window_sort_key(b)));
    ordered
        .into_iter()
        .map(|w| {
            // Past its reset the window is open again, whatever was last read of it.
            let reset_passed = w.resets_at.as_ref().map(|r| crate::app::millis_from_iso(r.as_str())).is_some_and(|at| at > 0 && now_ms > 0 && at <= now_ms);
            let used = if reset_passed { 0.0 } else { w.used_percent.as_f64().unwrap_or(0.0).clamp(0.0, 100.0) };
            let percent = used.round() as u32;
            let resets = w.resets_at.as_ref().filter(|_| !reset_passed).map(|r| crate::app::until_label(now_ms, r.as_str())).unwrap_or_default();
            let elapsed = if reset_passed { None } else { elapsed_share(now_ms, w) };
            LimitWindowView {
                label: w.label.clone(),
                percent,
                resets,
                high: percent >= 75,
                elapsed_percent: elapsed.map_or(0, |e| (e * 100.0).round() as u32),
                pace: elapsed.map(|e| pace_of(used, e)).unwrap_or_default().into(),
                resets_at: w.resets_at.as_ref().filter(|_| !reset_passed).map(|r| r.as_str().to_owned()).unwrap_or_default(),
            }
        })
        .collect()
}

/// How far through its period a window is, 0–1:
/// known only with both its length and when it refills.
fn elapsed_share(now_ms: i64, w: &domovoi_model::typed::ServerProviderUsageWindow) -> Option<f64> {
    let length = w.window_duration_mins.filter(|m| *m > 0)? as f64 * 60_000.0;
    let resets = crate::app::millis_from_iso(w.resets_at.as_ref()?.as_str());
    if resets == 0 || now_ms == 0 {
        return None;
    }
    Some(((length - (resets - now_ms) as f64) / length).clamp(0.0, 1.0))
}

/// Used against time gone, within 5 points of each other `on`.
fn pace_of(used_percent: f64, elapsed: f64) -> &'static str {
    let gap = used_percent - elapsed * 100.0;
    if gap > 5.0 {
        "ahead"
    } else if gap < -5.0 {
        "under"
    } else {
        "on"
    }
}

/// Why a provider's limits can't be shown; empty when they can.
fn limits_notice(limits: &domovoi_model::typed::ServerProviderUsageLimits) -> String {
    use domovoi_model::typed::ServerProviderUsageUnavailableReason as Reason;
    let message = |fallback: &str| limits.unavailable.as_ref().and_then(|u| u.message.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| fallback.to_owned());
    match limits.unavailable.as_ref().map(|u| &u.reason) {
        Some(Reason::Unsupported) => message("This account has no subscription limits."),
        Some(Reason::ProbeFailed) => message("Could not read limits."),
        _ if limits.windows.is_empty() => "No limits reported.".into(),
        _ => String::new(),
    }
}

/// A provider's account windows, when it reports any (or where to see them,
/// or a failed probe to tell the user about).
fn limits_of(now_ms: i64, p: &domovoi_model::typed::ServerProvider) -> Option<ProviderLimitsView> {
    let limits = p.usage_limits.as_ref()?;
    let windows = limit_windows(now_ms, Some(limits));
    let external = limits.external_usage.as_ref();
    let probe_failed = limits.unavailable.as_ref().is_some_and(|u| u.reason == ServerProviderUsageUnavailableReason::ProbeFailed);
    (!windows.is_empty() || external.is_some() || probe_failed).then(|| ProviderLimitsView {
        instance_id: p.instance_id.as_str().to_owned(),
        provider: provider_name(p),
        notice: if windows.is_empty() { limits_notice(limits) } else { String::new() },
        windows,
        external_label: external.map(|e| e.label.clone()).unwrap_or_default(),
        external_url: external.map(|e| e.url.clone()).unwrap_or_default(),
        checked: age_label(now_ms, limits.checked_at.as_str()),
    })
}

fn provider_name(p: &domovoi_model::typed::ServerProvider) -> String {
    p.display_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| p.instance_id.as_str().to_owned())
}

/// Every provider's account windows (the ones that report any).
fn provider_limits(model: &Model) -> Vec<ProviderLimitsView> {
    model.providers.iter().filter_map(|p| limits_of(model.shown_ms, p)).collect()
}

/// The open thread's context window and the limits of the provider it runs
/// on, for the meter in its header; `None` when neither is known.
fn session_usage(model: &Model, page: &ThreadPageView) -> Option<SessionUsageView> {
    let provider = model.providers.iter().find(|p| !page.provider_instance_id.is_empty() && p.instance_id.as_str() == page.provider_instance_id);
    let reported = provider.and_then(|p| p.usage_limits.as_ref());
    let mut limits = provider.and_then(|p| limits_of(model.shown_ms, p));
    let limits_notice = reported.map(limits_notice).unwrap_or_default();
    if limits.is_none() && !limits_notice.is_empty() && reported.is_some_and(|l| l.unavailable.is_some()) {
        // Unreadable: say so under the provider's name.
        limits = provider.map(|p| ProviderLimitsView {
            instance_id: p.instance_id.as_str().to_owned(),
            provider: provider_name(p),
            checked: reported.map(|l| age_label(model.shown_ms, l.checked_at.as_str())).unwrap_or_default(),
            notice: limits_notice.clone(),
            ..Default::default()
        });
    }
    if page.context.is_none() && limits.as_ref().is_none_or(|l| l.windows.is_empty()) {
        return None;
    }
    let limits_notice = if limits.is_some() { limits_notice } else { String::new() };
    let fullest = limits.as_ref().and_then(|l| l.windows.iter().max_by_key(|w| w.percent).cloned());
    let limit_alert = match fullest.as_ref().map(|w| w.percent) {
        Some(90..) => "danger",
        Some(75..) => "caution",
        _ => "",
    };
    let limits_checked = reported.filter(|_| limits.is_some()).map(|l| age_label(model.shown_ms, l.checked_at.as_str())).unwrap_or_default();
    Some(SessionUsageView { context: page.context.clone(), limits, fullest, limit_alert: limit_alert.into(), limits_notice, limits_checked })
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

/// The commands matching a draft that is `/` and one word, the app's own
/// first and then the provider's (its `/clear` is the app's): names starting
/// with the word first, then ones containing it.
fn slash_commands(model: &Model, instance_id: &str, draft: &str) -> Vec<SlashCommandView> {
    let Some(query) = draft.strip_prefix('/').filter(|q| !q.contains(char::is_whitespace)) else { return vec![] };
    let query = query.to_lowercase();
    let provider = model.providers.iter().find(|p| p.instance_id.as_str() == instance_id);
    let in_thread = model.selected_thread.is_some();
    let plan_mode = provider.is_none_or(|p| p.show_interaction_mode_toggle != Some(false));
    let builtins = crate::update::BUILTIN_COMMANDS
        .iter()
        .filter(|(name, _, _)| match *name {
            "fork" | "clear" => in_thread,
            "plan" => plan_mode,
            _ => true,
        })
        .map(|(name, hint, description)| SlashCommandView { name: (*name).into(), description: (*description).into(), kind: "builtin".into(), hint: (*hint).into() });
    let taken = |name: &str| crate::update::BUILTIN_COMMANDS.iter().any(|(n, _, _)| *n == name);
    let provided = provider
        .and_then(|p| p.slash_commands.as_ref())
        .into_iter()
        .flatten()
        .filter(|c| !taken(c.name.trim_start_matches('/')))
        .map(|c| SlashCommandView {
            name: c.name.trim_start_matches('/').to_owned(),
            description: c.description.clone().unwrap_or_default(),
            kind: "command".into(),
            hint: c.input.as_ref().map(|i| i.hint.clone()).unwrap_or_default(),
        });
    let all: Vec<SlashCommandView> = builtins.chain(provided).collect();
    if all.iter().any(|c| c.name.to_lowercase() == query) {
        return vec![]; // typed in full
    }
    let (mut starts, contains): (Vec<_>, Vec<_>) = all.into_iter().filter(|c| c.name.to_lowercase().contains(&query)).partition(|c| c.name.to_lowercase().starts_with(&query));
    starts.extend(contains);
    starts.truncate(SLASH_LIMIT);
    starts
}

/// The provider's skills matching a `$word` being typed at the draft's end
/// (the ones a user may invoke and that are on).
fn skills(model: &Model, instance_id: &str, draft: &str) -> Vec<SlashCommandView> {
    let Some((_, query)) = crate::update::skill_query(draft) else { return vec![] };
    let query = query.to_lowercase();
    let provider = model.providers.iter().find(|p| p.instance_id.as_str() == instance_id);
    let all: Vec<SlashCommandView> = provider
        .and_then(|p| p.skills.as_ref())
        .into_iter()
        .flatten()
        .filter(|s| s.enabled && s.user_invocable != Some(false))
        .filter(|s| query.is_empty() || s.name.to_lowercase().contains(&query) || s.display_name.as_deref().is_some_and(|d| d.to_lowercase().contains(&query)))
        .map(|s| SlashCommandView {
            name: s.name.clone(),
            description: s.short_description.clone().or_else(|| s.description.clone()).unwrap_or_default(),
            kind: "skill".into(),
            hint: s.scope.clone().unwrap_or_default(),
        })
        .collect();
    if all.iter().any(|s| s.name.to_lowercase() == query) {
        return vec![]; // typed in full
    }
    let (mut starts, contains): (Vec<_>, Vec<_>) = all.into_iter().partition(|s| s.name.to_lowercase().starts_with(&query));
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
    // The selected model's row, also when it was selected by an alias.
    let chosen_slug = selected.and_then(|s| update::model_record(model, &s.instance_id, &s.model).map(|(m, _)| (s.instance_id.clone(), m.slug.clone())));
    let is_selected = |o: &ModelOption| selected.is_some_and(|s| s.instance_id == o.instance_id && s.model == o.model) || chosen_slug.as_ref().is_some_and(|(i, slug)| *i == o.instance_id && *slug == o.model);
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
            chosen: is_selected(&o),
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
        let message = p.filter(|p| p.status != domovoi_model::typed::ServerProviderState::Ready).and_then(|p| p.message.clone()).unwrap_or_default();
        groups.push(ModelGroupView { instance_id: instance, provider, status, message, models, hidden_count });
    }
    // A provider that lists no models (signed out) still shows, with why.
    if query.is_empty() {
        for p in model.providers.iter().filter(|p| p.enabled && p.models.is_empty() && p.status != domovoi_model::typed::ServerProviderState::Ready) {
            let message = p.message.clone().unwrap_or_default();
            if message.is_empty() || groups.iter().any(|g| g.instance_id == p.instance_id.as_str()) {
                continue;
            }
            groups.push(ModelGroupView {
                instance_id: p.instance_id.as_str().to_owned(),
                provider: p.display_name.clone().unwrap_or_default(),
                status: p.status.as_str().to_owned(),
                message,
                models: vec![],
                hidden_count: 0,
            });
        }
    }
    (favorite_models, groups, count)
}

pub(crate) fn composer(model: &Model) -> ComposerView {
    use domovoi_model::typed::{OptionValue, ProviderOptionDescriptor};
    let selected = current_model(model);
    let (favorite_models, groups, model_count) = model_picker(model, selected.as_ref());
    let efforts = selected
        .as_ref()
        .and_then(|m| effort_descriptor(model, m))
        .map(|d| {
            // A thinking level not seen for this model: "Default" leaves it to the provider.
            let default = selected.as_ref().is_some_and(|m| crate::update::effort_default_allowed(model, m));
            default.then(|| ChoiceView { value: String::new(), label: "Default".into() }).into_iter().chain(d.options().iter().map(|o| ChoiceView { value: o.id.clone(), label: o.label.clone() })).collect()
        })
        .unwrap_or_default();
    let effort_label = selected
        .as_ref()
        .and_then(|m| effort_descriptor(model, m))
        .map(|d| match d.id() {
            Some("effort" | "reasoningEffort") => "Effort".to_owned(),
            _ => d.label().filter(|l| !l.trim().is_empty()).unwrap_or("Thinking").to_owned(),
        })
        .unwrap_or_default();
    let blocked = selected.as_ref().map(|m| crate::update::provider_blocked(model, &m.instance_id)).unwrap_or_default();
    let draft = model.draft();
    let is_new = model.selected_thread.is_none();
    let working = model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)).is_some_and(|t| matches!(shell_status(t), ShellStatus::Working | ShellStatus::Approval));
    let issue = crate::issues::draft(model);
    let workspace_required = issue.is_some_and(|draft| draft.workspace.is_none());
    let has_content = !draft.trim().is_empty() || !model.composer.attachments.is_empty() || issue.is_some();
    let slash = selected.as_ref().map(|m| slash_commands(model, &m.instance_id, &draft)).unwrap_or_default();
    let limits = selected
        .as_ref()
        .and_then(|m| model.providers.iter().find(|p| p.instance_id.as_str() == m.instance_id))
        .map(|p| limit_windows(model.shown_ms, p.usage_limits.as_ref()))
        .unwrap_or_default();
    let limits_chip = limits
        .iter()
        .max_by_key(|w| w.percent)
        .filter(|w| w.high)
        .map(|w| if w.resets.is_empty() { format!("{} {}%", w.label, w.percent) } else { format!("{} {}% · resets {}", w.label, w.percent, w.resets) })
        .unwrap_or_default();
    let traits = selected
        .as_ref()
        .map(|m| {
            crate::update::trait_descriptors(model, m)
                .into_iter()
                .filter_map(|d| {
                    let chosen = crate::update::chosen_trait(model, m, d);
                    match d {
                        ProviderOptionDescriptor::Boolean(b) => Some(TraitView {
                            id: b.id.clone(),
                            label: b.label.clone(),
                            description: b.description.clone().unwrap_or_default(),
                            kind: "switch".into(),
                            on: match chosen {
                                Some(OptionValue::Switch(on)) => on,
                                _ => b.current_value.unwrap_or(false),
                            },
                            ..Default::default()
                        }),
                        ProviderOptionDescriptor::Select(sel) => Some(TraitView {
                            id: sel.id.clone(),
                            label: sel.label.clone(),
                            description: sel.description.clone().unwrap_or_default(),
                            kind: "choice".into(),
                            value: match chosen {
                                Some(OptionValue::Choice(v)) => v,
                                _ => sel.current_value.clone().or_else(|| sel.options.iter().find(|o| o.is_default == Some(true)).or(sel.options.first()).map(|o| o.id.clone())).unwrap_or_default(),
                            },
                            choices: sel.options.iter().map(|o| ChoiceView { value: o.id.clone(), label: o.label.clone() }).collect(),
                            ..Default::default()
                        }),
                        ProviderOptionDescriptor::Other(_) => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let skill_list = selected.as_ref().map(|m| skills(model, &m.instance_id, &draft)).unwrap_or_default();
    // Stop was asked 8 s ago or more and the turn goes on.
    let stop_asked = model.composer.stop_asked_ms;
    ComposerView {
        limits,
        limits_chip,
        skills: skill_list,
        traits,
        stash: model.composer.stash.iter().map(|s| s.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().chars().take(80).collect()).collect(),
        can_force_stop: working
            && !model.composer.stop_asked_run.is_empty()
            && model.now_ms - stop_asked >= 8_000
            && model.selected_thread.as_deref().and_then(|t| model.thread_shell(t)).and_then(|t| t.active_run_id.as_ref()).is_some_and(|r| r.as_str() == model.composer.stop_asked_run),
        model_picker_requested: model.composer.model_picker_requested,
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
        selected_effort: selected.as_ref().and_then(|m| current_effort(model, m).or_else(|| crate::update::effort_default_allowed(model, m).then(String::new))),
        effort_label,
        selected_model: selected.clone(),
        efforts,
        runtime_modes: runtime_modes(),
        runtime_mode: composer_runtime_mode(model),
        interaction_mode: composer_interaction_mode(model),
        issue_chip: issue.map(|draft| draft.label.clone()),
        issue_available: is_new && crate::issues::available(model),
        issue_picker: if is_new { model.composer.issue_picker.clone() } else { None },
        issue_assign_to_me: issue.is_some_and(|draft| draft.assign),
        workspace_required,
        issue_handoff: model.composer.issue_handoff,
        workspace_mode: if let Some(issue) = issue { issue.workspace.clone().unwrap_or_default() } else if model.composer.workspace_mode.is_empty() { "local".into() } else { model.composer.workspace_mode.clone() },
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
            && !workspace_required
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

/// This machine's server, while offering it is of use: not when the app is
/// already using it (or signing in to it), and not for a saved entry that
/// only needs selecting, which the list of saved servers offers.
fn local_server(model: &Model) -> Option<LocalServerView> {
    let server = model.local_server.as_ref()?;
    let offered = match crate::environments::local_environment(model, server) {
        None => true,
        // Its entry is open and waits for a pairing code: one click instead.
        Some(entry) => model.active_env.as_deref() == Some(entry.id.as_str()) && model.token.is_none() && !model.in_flight && !model.local_signing_in(),
    };
    offered.then(|| LocalServerView { label: server.label.clone(), url: server.url.clone() })
}

/// The link waiting for the user's answer, as the question to ask.
fn link_request(model: &Model) -> Option<LinkRequestView> {
    let request = model.link_request.as_ref()?;
    let key = crate::environments::endpoint_key(&request.link.ticket);
    let endpoint = crate::environments::prefix(&key, 10);
    let link_name = request.link.name.as_deref().and_then(crate::environments::shown_name).unwrap_or_default();
    let known = model.environments.iter().find(|e| e.is_remote() && !model.unsaved.contains(&e.id) && crate::environments::endpoint_key(&e.ticket) == key);
    // The key and the names are the client's to lay out beside this (`endpoint`, `known_label`, `link_name`).
    let (title, message) = match known {
        Some(known) => {
            let sign_in = if request.link.code.is_some() { " It carries a pairing code, which signs this device in to it again." } else { "" };
            (format!("Connect to {}?", known.label), format!("A link asks this app to connect to a server saved here.{sign_in}"))
        }
        None => (
            "Connect to a new server?".to_owned(),
            "A link asks this app to connect to a server that is not saved here. Connect only if you made this link on your own server: a server you connect to sees what you send it, and this app shows what it sends back. The name in a link is whatever its author wrote.".to_owned(),
        ),
    };
    Some(LinkRequestView {
        id: request.id,
        title,
        message,
        confirm_label: "Connect".to_owned(),
        endpoint,
        link_name,
        known_label: known.map(|known| known.label.clone()).unwrap_or_default(),
    })
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

fn panel(model: &Model, collected_artifacts: &[crate::artifacts::CollectedArtifact]) -> PanelView {
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
    let detail = |id: &str| p.terminal_details.iter().find(|terminal| terminal.terminal_id == id && Some(terminal.thread_id.as_str()) == p.terminal_owner.as_deref());
    let screen = |session: &crate::terminal::Session| {
        let mut view = session.screen_view();
        if let Some(info) = detail(&session.id) {
            view.restored = info.restored;
            view.has_saved_screen = info.has_saved_screen;
        }
        view
    };
    let split = p.terminal_splits.iter().find(|split| Some(split.environment.as_str()) == model.active_env.as_deref()
        && Some(split.thread.as_str()) == model.selected_thread.as_deref()).and_then(|split| {
        let active = p.active_terminal.as_ref()?;
        let mut pair = p.terminals.iter().enumerate().filter(|(_, terminal)| terminal.id != *active);
        let (other_at, other) = pair.next()?;
        let (active_at, active) = p.terminals.iter().enumerate().find(|(_, terminal)| terminal.id == *active)?;
        let (left, right) = if active_at < other_at { (active, other) } else { (other, active) };
        Some(TerminalSplitView { stacked: split.stacked, percent: split.percent, left: screen(left), right: screen(right) })
    });
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
            can_rename: p.terminal_details_available,
            split,
            tabs: p.terminals.iter().map(|t| TerminalTabView { id: t.id.clone(), label: detail(&t.id).map(|info| {
                let generated = format!("Terminal {}", t.id.trim_start_matches("term-"));
                if info.label != generated { info.label.clone() } else { info.title.clone().unwrap_or_else(|| info.label.clone()) }
            }).unwrap_or_else(|| t.label.clone()), running: t.running(), active: p.active_terminal.as_deref() == Some(t.id.as_str()) }).collect(),
            active: p.active_terminal.as_deref().and_then(|id| p.terminals.iter().find(|t| t.id == id)).map(screen),
        },
        files: FilesPanelView {
            root: p.files.root.clone(),
            entries,
            loading: p.files.loading.contains(""),
            query: p.files.query.clone(),
            results: p.files.results.iter().map(entry_view).collect(),
            artifact_preview: p.files.artifact_preview.as_ref().filter(|read| read.current(model)).map(|read| {
                let pv = &read.preview;
                FilePreviewView { path: pv.path.clone(), spans: vec![], text: pv.text.clone(), image_url: None, truncated: pv.truncated, error: pv.error.clone(), loading: pv.loading, editable: false, size_label: None, lfs: None }
            }),
            preview: p.files.preview.as_ref().map(|pv| FilePreviewView {
                path: pv.path.clone(),
                spans: pv.spans.clone(),
                text: pv.text.clone(),
                image_url: pv.image_url.as_ref().map(|url| format!("{}{url}", model.server_url)),
                truncated: pv.truncated,
                error: pv.error.clone(),
                loading: pv.loading,
                editable: !pv.loading && !pv.truncated && pv.error.is_none() && pv.image_url.is_none() && pv.lfs.is_none(),
                size_label: (!pv.loading && pv.error.is_none()).then(|| bytes_label(pv.byte_length as f64)),
                lfs: pv.lfs.as_ref().map(|lfs| lfs_view(pv, lfs)),
            }),
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
            worktrees: model
                .composer
                .worktrees
                .iter()
                .filter(|(_, path)| model.projects.iter().all(|p| p.workspace_root != *path))
                .map(|(branch, path)| WorktreeView {
                    path: path.clone(),
                    branch: branch.clone(),
                    current: *path == p.git.cwd,
                    threads: model.threads.iter().filter(|t| t.deleted_at.is_none() && t.worktree_path.as_deref() == Some(path.as_str())).map(|t| t.title.clone()).collect(),
                })
                .collect(),
            scripts: model
                .selected_thread
                .as_deref()
                .and_then(|t| model.thread_shell(t))
                .map(|t| t.project_id.as_str().to_owned())
                .or_else(|| model.selected_project.clone())
                .and_then(|id| model.project(&id))
                .map(|project| project.scripts.iter().map(|s| ScriptView { id: s.id.clone(), name: s.name.clone(), command: s.command.clone(), icon: s.icon.as_str().to_owned() }).collect())
                .unwrap_or_default(),
            busy: p.git.busy,
            progress: p.git.progress.clone(),
            error: p.git.error.clone(),
            has_remote: local.is_some_and(|local| local.has_primary_remote),
        },
        artifacts: artifacts_panel(model, collected_artifacts),
    }
}

fn artifacts_panel(model: &Model, collected: &[crate::artifacts::CollectedArtifact]) -> ArtifactsPanelView {
    let items: Vec<ArtifactSummaryView> = collected.iter().map(crate::artifacts::CollectedArtifact::summary_view).collect();
    let state = &model.panel.artifacts;
    let tid = model.selected_thread.as_deref().unwrap_or_default();
    let mode = if state.mode == "code" { "code" } else { "preview" }.to_owned();

    let active = state.active_id.as_deref().filter(|_| state.thread_id.as_deref() == Some(tid)).and_then(|active_id| {
        if let Some(art) = collected.iter().find(|a| a.id == active_id) {
            let ver = state
                .active_version
                .and_then(|n| art.versions.iter().find(|v| v.version == n))
                .or_else(|| art.latest())?;
            let file_read = state.file_read.as_ref().filter(|r| r.current(model) && ver.path.as_deref() == Some(r.preview.path.as_str()));
            let (source, spans, loading_file, truncated, file_error) = match &ver.source {
                Some(src) => (
                    src.clone(),
                    crate::highlight::highlight(src, &art.language),
                    false,
                    false,
                    None,
                ),
                None => match file_read {
                    Some(read) => (
                        read.preview.text.clone(),
                        read.preview.spans.clone(),
                        read.preview.loading,
                        read.preview.truncated,
                        read.preview.error.clone(),
                    ),
                    None => (String::new(), vec![], ver.path.is_some(), false, None),
                },
            };
            let url_state = state.preview_url.as_ref().filter(|u| u.matches(tid, &art.id, ver.version));
            let preview_url = url_state
                .and_then(|u| u.url.as_deref())
                .map(|rel| format!("{}{rel}", model.server_url));
            let loading_url = url_state.is_some_and(|u| u.loading) && matches!(art.kind.as_str(), "react");
            let error = file_error.or_else(|| {
                if art.kind == "react" {
                    url_state.and_then(|u| u.error.clone())
                } else {
                    None
                }
            });
            Some(ActiveArtifactView {
                id: art.id.clone(),
                title: art.title.clone(),
                kind: art.kind.clone(),
                language: art.language.clone(),
                version: ver.version,
                versions: art.version_views(),
                mode: mode.clone(),
                source,
                spans,
                preview_url,
                hosted_url: ver.hosted_url.clone().or_else(|| art.hosted_url.clone()),
                path: ver.path.clone().or_else(|| art.path.clone()),
                item_id: ver.item_id.clone(),
                loading: loading_file || loading_url,
                truncated,
                streaming: art.streaming,
                error,
            })
        } else if let Some(path) = active_id.strip_prefix("file:") {
            // Workspace artifact opened directly (e.g. from Files pane) not yet listed in transcript.
            let kind = crate::artifacts::artifact_kind_for_path(path).unwrap_or("html");
            let file_read = state.file_read.as_ref().filter(|r| r.current(model) && r.preview.path == path);
            let url_state = state.preview_url.as_ref().filter(|u| u.matches(tid, active_id, 1));
            let preview_url = url_state
                .and_then(|u| u.url.as_deref())
                .map(|rel| format!("{}{rel}", model.server_url));
            Some(ActiveArtifactView {
                id: active_id.to_owned(),
                title: crate::artifacts::extract_title(kind, "", Some(path)),
                kind: kind.to_owned(),
                language: crate::highlight::language_for_path(path),
                version: 1,
                versions: vec![ArtifactVersionView { version: 1, summary: String::new(), item_id: String::new() }],
                mode: mode.clone(),
                source: file_read.map(|r| r.preview.text.clone()).unwrap_or_default(),
                spans: file_read.map(|r| r.preview.spans.clone()).unwrap_or_default(),
                preview_url,
                hosted_url: None,
                path: Some(path.to_owned()),
                item_id: String::new(),
                loading: file_read.is_none_or(|r| r.preview.loading),
                truncated: file_read.is_some_and(|r| r.preview.truncated),
                streaming: false,
                error: file_read.and_then(|r| r.preview.error.clone()),
            })
        } else {
            None
        }
    });

    ArtifactsPanelView { items, active }
}

/// Palette entries for the current query: actions, threads, projects.
pub fn palette_items(model: &Model) -> Vec<PaletteItemView> {
    let Some(p) = &model.palette else { return vec![] };
    if matches!(p.mode.as_str(), "files" | "content") {
        return p.results.clone();
    }
    let q = p.query.trim().to_lowercase();
    let hit = |t: &str| q.is_empty() || t.to_lowercase().contains(&q);
    let mut items = vec![];
    let thread_selected = model.selected_thread.is_some();
    let actions = [
        ("new-thread", "New thread", "⌘N", true),
        ("toggle-panel", "Toggle side panel", "⌘J", true),
        ("diff", "Show diff", "", true),
        ("artifacts", "Show artifacts", "⌘⇧A", thread_selected),
        ("terminal", "Show terminal", "", thread_selected),
        ("files", "Show files", "", true),
        ("git", "Show git", "", true),
        ("find", "Find in thread", "⌘F", thread_selected),
        ("go-to-file", "Go to file…", "", model.active_cwd().is_some()),
        ("search-project", "Search in project…", "", model.active_cwd().is_some()),
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
    let mut groups = vec![
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
            title: "Threads".into(),
            fields: vec![
                field_bool(model, "autoResumeLimitedThreads", "Resume after usage limits", "A thread stopped by a usage limit continues on its own when the limit resets."),
                field_bool(model, "snoozeLimitedThreads", "Snooze threads stopped by limits", "Hide a limited thread until its limit resets."),
                field_text(model, "threadTitleInstructions", "Title instructions", "How the server should name new threads. Empty uses its default."),
            ],
        },
        SettingGroupView {
            title: "Auto-settle".into(),
            fields: vec![
                field_text(model, "sidebarAutoSettleAfterDays", "Settle after days", "Threads with no activity move to Settled after this many days."),
                field_bool(model, "sidebarAutoSettleOnMerge", "Settle when the pull request merges", "Automatically settle a thread once its pull request merges."),
                field_bool(model, "sidebarAutoSettleKeepPullRequests", "Keep threads with open pull requests", "Do not auto-settle threads that still have an open pull request."),
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
                field_bool(model, "aiReviewEnabled", "AI code reviews", "Run evidence-checked AI code reviews on pull requests in a read-only snapshot."),
                field_choice(
                    model,
                    "aiReviewDepthMode",
                    "AI review depth",
                    "auto splits large PRs by import graph; economic uses a single pass; deep always chunks.",
                    &[("auto", "Auto"), ("economic", "Economic"), ("deep", "Deep")],
                ),
                field_bool(model, "aiReviewLensEnabled", "Second-pass Lens review", "Run a focused adversarial second pass over primary findings."),
                field_bool(model, "prWatcherEnabled", "Background PR watcher", "Poll watched pull requests and wake threads when checks settle or PRs merge."),
                field_bool(model, "prWatchWakeOnFailedChecks", "Agents fix failed checks", "When checks fail on a pull request made from a thread, wake its agent to fix them. On unless turned off."),
                field_text(model, "prWatchMaxFixAttempts", "Fix attempts in a row", "How many times in a row an agent is woken for one pull request's failed checks (3 unless set; 0 is no limit). Writing in the thread starts the count again."),
                field_bool(model, "failoverLimitedThreads", "Fail over rate-limited threads", "Hand off interactive threads to a healthy fallback provider instance when a usage limit hits."),
            ],
        },
    ];
    if model.panel.terminal_details_available {
        groups.push(SettingGroupView {
            title: "Terminal".into(),
            fields: vec![field_bool(model, "saveTerminalScreens", "Save the last terminal screen",
                "Stores the last visible screen on the server, where it can be viewed after a restart.")],
        });
    }
    groups
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
                format!("{os}Signed in {}", age_label(model.shown_ms, c.issued_at.as_str()))
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
        linear_settings: crate::issues::linear_settings(model),
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
                limits: limits_label(model.shown_ms, p.usage_limits.as_ref()),
                can_authenticate: p.setup.as_ref().is_some_and(|s| s.can_authenticate),
                can_install: p.setup.as_ref().is_some_and(|s| s.can_install),
                signed_in: p.auth.status == domovoi_model::typed::ServerProviderAuthStatus::Authenticated,
                sign_in: sign_in_view(pages.provider_auth.get(p.instance_id.as_str())),
                install: install_view(pages.provider_install.get(p.instance_id.as_str())),
                config: provider_config_view(model, p),
            })
            .collect(),
        can_add_antigravity: model.providers.iter().any(|p| p.driver.as_str() == "antigravity"),
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
                    status => format!("{} {}", status.as_str(), age_label(model.shown_ms, t.last_run_at.as_ref().map_or("", Timestamp::as_str))),
                },
                project_title: model.project(t.project_id.as_str()).map(|p| p.title.clone()).unwrap_or_default(),
                project_id: t.project_id.as_str().to_owned(),
                schedule_kind: match &t.schedule {
                    ScheduledTaskSchedule::Interval(_) => "interval",
                    ScheduledTaskSchedule::FixedTime(_) => "fixed_time",
                    ScheduledTaskSchedule::Once(_) => "once",
                    ScheduledTaskSchedule::Webhook(_) => "webhook",
                    ScheduledTaskSchedule::Other(_) => "",
                }
                .to_owned(),
                interval_minutes: match &t.schedule {
                    ScheduledTaskSchedule::Interval(i) => u32::try_from(i.every_ms / 60_000).unwrap_or(u32::MAX),
                    _ => 60,
                },
                time_of_day: match &t.schedule {
                    ScheduledTaskSchedule::FixedTime(f) => f.time_of_day.clone(),
                    _ => String::new(),
                },
                weekdays: match &t.schedule {
                    ScheduledTaskSchedule::FixedTime(f) => f.weekdays.iter().flatten().filter_map(|d| u8::try_from(*d).ok()).collect(),
                    _ => vec![],
                },
                run_at: match &t.schedule {
                    ScheduledTaskSchedule::Once(o) => o.run_at.as_str().to_owned(),
                    _ => String::new(),
                },
                thread_mode: t.thread_mode.as_ref().map_or_else(|| if t.thread_id.is_some() { "continue".into() } else { "new_thread".into() }, |m| m.as_str().to_owned()),
                thread_id: t.thread_id.as_ref().map(|id| id.as_str().to_owned()).unwrap_or_default(),
                thread_title: t.thread_id.as_ref().and_then(|id| model.thread_shell(id.as_str())).map(|shell| shell.title.clone()).unwrap_or_default(),
                run_state: pages.scheduled_latest_runs.iter().find(|r| r.task_id == t.id.as_str()).map(|r| match r.status.as_str() {
                    "queued" => "Waiting in its thread".to_owned(),
                    "running" => "Running".to_owned(),
                    "waiting" => "Needs you".to_owned(),
                    _ => String::new(),
                }).unwrap_or_default(),
                last_thread_id: pages.scheduled_latest_runs.iter().find(|r| r.task_id == t.id.as_str()).and_then(|r| r.thread_id.clone()).unwrap_or_default(),
                workspace: if matches!(t.workspace_strategy, domovoi_model::typed::WorkspaceStrategy::Worktree { .. }) { "worktree".into() } else { "root".into() },
                base_branch: match &t.workspace_strategy {
                    domovoi_model::typed::WorkspaceStrategy::Worktree { base_ref, .. } => base_ref.clone(),
                    _ => String::new(),
                },
                model_instance: t.model_selection.instance_id.as_str().to_owned(),
                model: t.model_selection.model.clone(),
                runtime_mode: t.runtime_mode.as_str().to_owned(),
                interaction_mode: t.interaction_mode.as_str().to_owned(),
                last_error: t.last_run_error.clone().unwrap_or_default(),
                webhook_url: t.webhook.as_ref().map(|w| w.url.clone().unwrap_or_else(|| w.path.clone())).unwrap_or_default(),
            })
            .collect(),
        // Read through every thread: only while the scheduled tasks are open
        // (their screen, or an editor from a thread, subscribes).
        scheduled_thread_choices: if model.rpc.has(&crate::rpc::Route::ScheduledTasks) { scheduled_thread_choices(model) } else { vec![] },
        scheduled_time_zone: pages.scheduled_time_zone.clone(),
        scheduled_runs_task: pages.scheduled_runs.as_ref().map(|(id, _)| id.clone()).unwrap_or_default(),
        scheduled_runs: pages.scheduled_runs.iter().flat_map(|(_, runs)| runs).map(|r| scheduled_run_view(model, r)).collect(),
        webhook_deliveries_task: pages.webhook_deliveries.as_ref().map(|(id, _)| id.clone()).unwrap_or_default(),
        webhook_deliveries: pages
            .webhook_deliveries
            .iter()
            .flat_map(|(_, list)| list.iter())
            .map(|d| WebhookDeliveryView {
                id: d.id.clone(),
                received: age_label(model.shown_ms, &d.received_at),
                method: d.method.clone(),
                outcome: d.outcome.replace('_', " "),
                detail: d.error.clone().unwrap_or_else(|| if d.missing_fields.is_empty() { String::new() } else { format!("Missing: {}", d.missing_fields.join(", ")) }),
            })
            .collect(),
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
                    age_label(model.shown_ms, cleanup.finished_at.as_str())
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
        project_icon_files: crate::project_icons::files_view(model),
        projects: model
            .projects
            .iter()
            .map(|p| {
                let idx = pages.project_indexes.get(p.id.as_str());
                ProjectSettingsView {
                linear_scope: model.pages.issue_trackers.bindings.get(p.id.as_str()).filter(|binding| binding.tracker == "linear").map(|binding| binding.scope.clone()),
                    id: p.id.as_str().to_owned(),
                    title: p.title.clone(),
                    workspace_root: p.workspace_root.clone(),
                    icon: crate::project_icons::view(model, p, false),
                    automatic_icon: crate::project_icons::view(model, p, true),
                    thread_count: model.threads.iter().filter(|t| t.project_id == p.id && t.deleted_at.is_none()).count() as u32,
                    is_scratch: p.workspace_root.ends_with("/scratch"),
                    index: ProjectIndexView {
                        status: idx.map(|s| s.status.clone()).unwrap_or_else(|| "idle".into()),
                        commit_sha: idx.and_then(|s| s.commit_sha.clone()).unwrap_or_default(),
                        indexed_at: idx.and_then(|s| s.indexed_at.clone()).unwrap_or_default(),
                        file_count: idx.map_or(0, |s| narrow(s.file_count)),
                        package_count: idx.map_or(0, |s| narrow(s.package_count)),
                        edge_count: idx.map_or(0, |s| narrow(s.edge_count)),
                        symbol_count: idx.map_or(0, |s| narrow(s.symbol_count)),
                        convention_paths: idx.map(|s| s.convention_paths.clone()).unwrap_or_default(),
                        error: idx.and_then(|s| s.error.clone()),
                    },
                }
            })
            .collect(),
        failover_pools: pages
            .failover_status
            .as_ref()
            .map(|st| &st.pools)
            .or_else(|| model.settings.as_ref().and_then(|s| s.failover_pools.as_ref()))
            .into_iter()
            .flatten()
            .map(|pool| FailoverPoolView {
                id: pool.id.clone(),
                name: pool.name.clone(),
                instance_ids: pool.instance_ids.iter().map(|id| id.as_str().to_owned()).collect(),
                same_driver_first: pool.same_driver_first,
                allow_cross_provider: pool.allow_cross_provider,
            })
            .collect(),
        failover_instances: pages
            .failover_status
            .iter()
            .flat_map(|st| &st.instances)
            .map(|inst| FailoverInstanceView {
                instance_id: inst.instance_id.clone(),
                driver: inst.driver.clone(),
                display_name: inst.display_name.clone(),
                available: inst.available,
                limited_until: inst.limited_until.clone(),
                last_error: inst.last_error.clone(),
                active_jobs: inst.active_jobs,
            })
            .collect(),
    }
}

/// A scheduled task's run as its history shows it.
fn scheduled_run_view(model: &Model, r: &crate::model::ScheduledRun) -> ScheduledRunView {
    let at = r.started_at.replace('T', " ").chars().take(16).collect::<String>();
    let ago = age_label(model.shown_ms, &r.started_at);
    let thread_id = r.thread_id.clone().unwrap_or_default();
    ScheduledRunView {
        started: if ago.is_empty() { at } else { format!("{ago} · {at}") },
        trigger: match r.trigger.as_str() {
            "manual" => "Run now",
            "webhook" => "Webhook",
            _ => "On schedule",
        }
        .into(),
        status: match r.status.as_str() {
            "queued" => "Waiting in its thread",
            "running" => "Running",
            "waiting" => "Needs you",
            "skipped" => "Skipped",
            "completed" => "Finished",
            "stopped" => "Stopped",
            "failed" => "Failed",
            _ => "Sent",
        }
        .into(),
        failed: r.status == "failed",
        error: r.error.clone().unwrap_or_default(),
        thread_title: model.thread_shell(&thread_id).map(|t| t.title.clone()).unwrap_or_default(),
        thread_id,
    }
}

/// The threads a scheduled task can post into: every live thread (snoozed and
/// settled ones too), newest first, then the threads tasks are bound to that
/// are no longer live, so an edit still names its thread.
fn scheduled_thread_choices(model: &Model) -> Vec<ScheduledThreadChoiceView> {
    let labels = crate::sidebar::Labels::new(model);
    let mut live: Vec<&ThreadShell> = crate::sidebar::live_threads(model).collect();
    live.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
    let mut choices: Vec<ScheduledThreadChoiceView> = live
        .iter()
        .map(|t| {
            let age = age_label(model.shown_ms, t.updated_at.as_str());
            let status = crate::sidebar::status_label(model, t);
            ScheduledThreadChoiceView {
                id: t.id.as_str().to_owned(),
                project_id: t.project_id.as_str().to_owned(),
                title: t.title.clone(),
                detail: [status, age].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
                model_label: labels.model_label(t),
                runtime_mode: t.runtime_mode.as_str().to_owned(),
            }
        })
        .collect();
    for task in &model.pages.scheduled {
        let Some(id) = task.thread_id.as_ref().map(|id| id.as_str()) else { continue };
        if choices.iter().any(|c| c.id == id) {
            continue;
        }
        let shell = model.thread_shell(id);
        choices.push(ScheduledThreadChoiceView {
            id: id.to_owned(),
            project_id: shell.map_or_else(|| task.project_id.as_str().to_owned(), |t| t.project_id.as_str().to_owned()),
            title: shell.map_or_else(|| "A thread not on this device".into(), |t| t.title.clone()),
            detail: if shell.is_some_and(|t| t.archived_at.is_some()) { "archived".into() } else { String::new() },
            model_label: shell.map(|t| labels.model_label(t)).unwrap_or_default(),
            runtime_mode: shell.map(|t| t.runtime_mode.as_str().to_owned()).unwrap_or_default(),
        });
    }
    choices
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

/// A provider's usage windows in a line; the fullest window says when it resets.
fn limits_label(now_ms: i64, limits: Option<&domovoi_model::typed::ServerProviderUsageLimits>) -> String {
    let Some(limits) = limits else { return String::new() };
    let windows: Vec<_> = limits.windows.iter().collect();
    if windows.is_empty() {
        return String::new();
    }
    let used = |w: &domovoi_model::typed::ServerProviderUsageWindow| w.used_percent.as_f64().unwrap_or(0.0);
    let mut line = windows.iter().map(|w| format!("{} {:.0}%", w.label, used(w))).collect::<Vec<_>>().join(" · ");
    if let Some(fullest) = windows.iter().max_by(|a, b| used(a).total_cmp(&used(b))).filter(|w| used(w) >= 50.0)
        && let Some(reset) = &fullest.resets_at
    {
        line.push_str(&format!(", resets {}", crate::app::until_label(now_ms, reset.as_str())));
    }
    line
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
        ScheduledTaskSchedule::FixedTime(fixed) => match fixed.weekdays.as_deref() {
            None | Some([]) => format!("Daily at {}", fixed.time_of_day),
            Some(days) if days == [1, 2, 3, 4, 5] => format!("Weekdays at {}", fixed.time_of_day),
            Some(days) => {
                const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
                let names: Vec<&str> = days.iter().filter_map(|d| NAMES.get(*d as usize).copied()).collect();
                format!("{} at {}", names.join(", "), fixed.time_of_day)
            }
        },
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
                age: age_label(model.shown_ms, e.updated_at.as_str()),
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
                ai_review: pr_review_view(p),
            }
        }),
    }
}

fn pr_review_view(p: &crate::model::Pages) -> Option<PrReviewView> {
    let res = p.pr_review.as_ref();
    let run = res.and_then(|r| r.run.as_ref());
    if run.is_none() && !p.pr_review_busy {
        return None;
    }
    let Some(run) = run else {
        return Some(PrReviewView {
            status: "running".into(),
            phase: "starting".into(),
            phase_message: p.pr_review_phase_message.clone(),
            busy: true,
            ..Default::default()
        });
    };
    let findings: Vec<PrReviewFindingView> = res
        .into_iter()
        .flat_map(|r| &r.findings)
        .map(|f| PrReviewFindingView {
            id: f.id.clone(),
            kind: f.kind.clone(),
            severity: f.severity.clone(),
            title: f.title.clone(),
            detail: f.detail.clone(),
            action: f.action.clone(),
            path: f.path.clone().unwrap_or_default(),
            line: f.line.map_or(0, narrow),
            side: f.side.clone().unwrap_or_default(),
            evidence_quotes: f.evidence.iter().map(|e| format!("{}:{} — {}", e.path, e.line, e.quote)).collect(),
            selected: f.selected,
            source_stage: f.source_stage.clone(),
        })
        .collect();
    let selected_count = findings.iter().filter(|f| f.selected).count() as u32;
    let current_head = res.and_then(|r| r.current_head_sha.as_deref());
    let is_stale = current_head.is_some_and(|h| !h.is_empty() && h != run.head_sha);
    let busy = p.pr_review_busy || matches!(run.status.as_str(), "queued" | "running");
    Some(PrReviewView {
        run_id: run.id.clone(),
        status: run.status.clone(),
        phase: run.phase.clone(),
        phase_message: p.pr_review_phase_message.clone(),
        scope_mode: run.scope_mode.clone(),
        head_sha: run.head_sha.clone(),
        is_stale,
        risk_score: run.risk_score,
        risk_drivers: run
            .risk_drivers
            .iter()
            .map(|d| PrReviewRiskDriverView { points: d.points as i32, label: d.label.clone(), detail: d.detail.clone() })
            .collect(),
        summary: run.summary_markdown.clone().unwrap_or_default(),
        findings,
        selected_count,
        published_review_id: run.published_review_id.clone(),
        error: run.error_detail.clone(),
        can_publish: run.status == "validated" && !is_stale && !busy,
        busy,
    })
}

fn usage_provider_label(key: &str) -> String {
    match key {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "pi" => "Pi".into(),
        "antigravity" => "Antigravity".into(),
        "cursor" => "Cursor".into(),
        "grok" => "Grok".into(),
        "opencode" => "OpenCode".into(),
        "muse" => "Muse".into(),
        other => other.to_owned(),
    }
}

fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let mut parts = s.split('-');
    let y = parts.next()?.parse::<i32>().ok()?;
    let m = parts.next()?.parse::<u32>().ok()?;
    let d = parts.next()?.parse::<u32>().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) { Some((y, m, d)) } else { None }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
        _ => 30,
    }
}

fn enumerate_days(since: &str, until: &str) -> Vec<String> {
    let (Some(mut cur), Some(end)) = (parse_ymd(since), parse_ymd(until)) else { return vec![] };
    let mut out = vec![];
    while cur <= end && out.len() < 400 {
        out.push(format!("{:04}-{:02}-{:02}", cur.0, cur.1, cur.2));
        cur.2 += 1;
        if cur.2 > days_in_month(cur.0, cur.1) {
            cur.2 = 1;
            cur.1 += 1;
            if cur.1 > 12 {
                cur.1 = 1;
                cur.0 += 1;
            }
        }
    }
    out
}

fn short_day_label(day: &str) -> (String, String) {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let Some((y, m, d)) = parse_ymd(day) else { return (day.to_owned(), day.to_owned()) };
    let mon = MONTHS.get((m - 1) as usize).copied().unwrap_or("");
    (format!("{mon} {d}"), format!("{mon} {d}, {y}"))
}

/// Local hour `0..24` for an RFC 3339 `hourStart` string, shifted by `utc_offset_secs`.
fn local_hour_of(hour_start: &str, utc_offset_secs: i32) -> Option<u32> {
    let ms = crate::app::millis_from_iso(hour_start);
    if ms == 0 {
        return None;
    }
    let shifted = ms.saturating_add(i64::from(utc_offset_secs) * 1_000);
    let secs = shifted.div_euclid(1_000);
    Some((secs.div_euclid(3_600).rem_euclid(24)) as u32)
}

fn share_pct(part: u64, whole: u64) -> u32 {
    if whole == 0 || part == 0 {
        0
    } else {
        ((part as f64 / whole as f64) * 100.0).round().min(100.0) as u32
    }
}

fn usage(model: &Model) -> UsageView {
    let summary = model.pages.usage.as_ref();
    let priced = summary.is_some_and(|summary| summary.pricing.status != UsagePricingStatus::Unavailable);
    let cost_of = |usd: f64| if priced { money_label(usd, "USD") } else { String::new() };
    let days = if model.pages.usage_days == 0 { 7 } else { model.pages.usage_days };
    let selected_provider = model.pages.usage_provider_filter.clone();

    // First pass across every bucket: provider totals (unfiltered, so filter chips stay visible).
    let mut provider_totals: Vec<(String, u64, f64)> = vec![];
    let mut unfiltered_total = 0u64;
    let mut has_hour_buckets = false;
    for b in summary.into_iter().flat_map(|s| &s.buckets) {
        let t = &b.totals;
        let sum = t.uncached_input_tokens + t.cached_input_tokens + t.cache_creation_tokens + t.output_tokens + t.reasoning_tokens;
        let usd = b.cost_usd.as_f64().unwrap_or(0.0);
        unfiltered_total += sum;
        if b.hour_start.as_ref().and_then(|h| h.as_deref()).is_some() {
            has_hour_buckets = true;
        }
        let key = b.provider.as_str();
        match provider_totals.iter_mut().find(|(k, _, _)| k == key) {
            Some((_, tok, cost)) => {
                *tok += sum;
                *cost += usd;
            }
            None => provider_totals.push((key.to_owned(), sum, usd)),
        }
    }
    provider_totals.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let providers: Vec<UsageProviderSummaryView> = provider_totals
        .into_iter()
        .map(|(key, tokens, usd)| UsageProviderSummaryView {
            label: usage_provider_label(&key),
            key,
            tokens,
            tokens_label: token_label(tokens),
            share_percent: share_pct(tokens, unfiltered_total),
            cost: cost_of(usd),
        })
        .collect();

    let hourly = days == 1 && has_hour_buckets;

    // Second pass: filtered totals, by-model, series accumulators, and rows.
    let mut total_uncached = 0u64;
    let mut total_cached = 0u64;
    let mut total_cache_write = 0u64;
    let mut total_output = 0u64;
    let mut total_reasoning = 0u64;
    let mut total = 0u64;
    let mut total_usd = 0.0f64;
    let mut total_turns = 0u64;
    let mut active_day_set: FxHashSet<String> = FxHashSet::default();
    let mut active_hour_set: FxHashSet<u32> = FxHashSet::default();
    // (provider_key, model) -> ([uncached, cached, cache_write, output, reasoning, sum], sessions, usd)
    let mut by_model_acc: Vec<((String, String), [u64; 6], u64, f64)> = vec![];
    // slot_key -> (total_tokens, cost_usd, provider_key -> tokens)
    let mut slot_totals: HashMap<String, (u64, f64, Vec<(String, u64)>)> = HashMap::new();
    // Raw rows for sorting newest-first before converting to UsageRowView.
    let mut raw_rows: Vec<(String, String, String, String, [u64; 6], u64, f64)> = vec![];

    for b in summary.into_iter().flat_map(|s| &s.buckets) {
        let pkey = b.provider.as_str();
        if !selected_provider.is_empty() && pkey != selected_provider {
            continue;
        }
        let t = &b.totals;
        let sum = t.uncached_input_tokens + t.cached_input_tokens + t.cache_creation_tokens + t.output_tokens + t.reasoning_tokens;
        let usd = b.cost_usd.as_f64().unwrap_or(0.0);
        total_uncached += t.uncached_input_tokens;
        total_cached += t.cached_input_tokens;
        total_cache_write += t.cache_creation_tokens;
        total_output += t.output_tokens;
        total_reasoning += t.reasoning_tokens;
        total += sum;
        total_usd += usd;
        total_turns += b.records;
        if sum > 0 {
            active_day_set.insert(b.day.clone());
        }

        let hour_label = b
            .hour_start
            .as_ref()
            .and_then(|h| h.as_deref())
            .and_then(|hs| local_hour_of(hs, model.utc_offset_secs))
            .map(|h| {
                if sum > 0 {
                    active_hour_set.insert(h);
                }
                format!("{h:02}:00")
            })
            .unwrap_or_default();

        let slot_key = if hourly && !hour_label.is_empty() { hour_label.clone() } else { b.day.clone() };
        let entry = slot_totals.entry(slot_key).or_insert_with(|| (0, 0.0, vec![]));
        entry.0 += sum;
        entry.1 += usd;
        match entry.2.iter_mut().find(|(k, _)| k == pkey) {
            Some((_, tok)) => *tok += sum,
            None => entry.2.push((pkey.to_owned(), sum)),
        }

        let counts = [t.uncached_input_tokens, t.cached_input_tokens, t.cache_creation_tokens, t.output_tokens, t.reasoning_tokens, sum];
        let mkey = (pkey.to_owned(), b.model.clone());
        match by_model_acc.iter_mut().find(|(k, _, _, _)| *k == mkey) {
            Some((_, acc, sess, acc_usd)) => {
                acc.iter_mut().zip(counts).for_each(|(a, n)| *a += n);
                *sess += b.sessions;
                *acc_usd += usd;
            }
            None => by_model_acc.push((mkey, counts, b.sessions, usd)),
        }
        raw_rows.push((b.day.clone(), hour_label, pkey.to_owned(), b.model.clone(), counts, b.sessions, usd));
    }

    // Newest day/hour first, then highest token volume within the same day/hour.
    raw_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| b.4[5].cmp(&a.4[5])));

    let make_row = |day: String, hour_label: String, provider_key: String, model_name: String, c: [u64; 6], sessions: u64, usd: f64| -> UsageRowView {
        let total_in = c[0] + c[1] + c[2];
        UsageRowView {
            day,
            hour_label,
            provider: usage_provider_label(&provider_key),
            provider_key,
            model: model_name,
            input: token_label(c[0] + c[2]),
            cached: token_label(c[1]),
            cache_write: token_label(c[2]),
            output: token_label(c[3] + c[4]),
            reasoning: token_label(c[4]),
            total: token_label(c[5]),
            total_tokens: c[5],
            share_percent: share_pct(c[5], total),
            cache_hit_percent: share_pct(c[1], total_in),
            sessions,
            cost: cost_of(usd),
        }
    };

    let rows: Vec<UsageRowView> = raw_rows.into_iter().map(|(d, h, p, m, c, s, u)| make_row(d, h, p, m, c, s, u)).collect();
    by_model_acc.sort_by(|a, b| b.1[5].cmp(&a.1[5]));
    let by_model: Vec<UsageRowView> = by_model_acc.into_iter().map(|((p, m), c, s, u)| make_row(String::new(), String::new(), p, m, c, s, u)).collect();

    // Chronological, zero-filled time-series for charts.
    let series: Vec<UsageSeriesPointView> = if let Some(summary) = summary {
        if hourly {
            (0..24u32)
                .map(|h| {
                    let key = format!("{h:02}:00");
                    let detail_label = format!("{h:02}:00–{:02}:00", (h + 1) % 24);
                    let (tok, usd, mut by_p) = slot_totals.remove(&key).unwrap_or_default();
                    by_p.sort_by(|a, b| a.0.cmp(&b.0));
                    UsageSeriesPointView {
                        label: key.clone(),
                        key,
                        detail_label,
                        total_tokens: tok,
                        total_label: token_label(tok),
                        cost: cost_of(usd),
                        by_provider: by_p
                            .into_iter()
                            .filter(|(_, t)| *t > 0)
                            .map(|(pk, t)| UsageProviderPointView { provider: usage_provider_label(&pk), provider_key: pk, tokens: t })
                            .collect(),
                    }
                })
                .collect()
        } else {
            enumerate_days(&summary.since_day, &summary.until_day)
                .into_iter()
                .map(|day| {
                    let (label, detail_label) = short_day_label(&day);
                    let (tok, usd, mut by_p) = slot_totals.remove(&day).unwrap_or_default();
                    by_p.sort_by(|a, b| a.0.cmp(&b.0));
                    UsageSeriesPointView {
                        key: day,
                        label,
                        detail_label,
                        total_tokens: tok,
                        total_label: token_label(tok),
                        cost: cost_of(usd),
                        by_provider: by_p
                            .into_iter()
                            .filter(|(_, t)| *t > 0)
                            .map(|(pk, t)| UsageProviderPointView { provider: usage_provider_label(&pk), provider_key: pk, tokens: t })
                            .collect(),
                    }
                })
                .collect()
        }
    } else {
        vec![]
    };

    // Token mix slices.
    let token_mix: Vec<UsageSliceView> = [
        ("input", "Input", total_uncached),
        ("cached", "Cache read", total_cached),
        ("cache_write", "Cache write", total_cache_write),
        ("output", "Output", total_output),
        ("reasoning", "Reasoning", total_reasoning),
    ]
    .into_iter()
    .filter(|(_, _, n)| *n > 0)
    .map(|(kind, label, tokens)| UsageSliceView {
        kind: kind.into(),
        label: label.into(),
        tokens,
        tokens_label: token_label(tokens),
        percent: share_pct(tokens, total),
    })
    .collect();

    let total_input = total_uncached + total_cached + total_cache_write;
    let cache_hit_percent = share_pct(total_cached, total_input);
    let cache_hit_label = if total_input > 0 { format!("{cache_hit_percent}%") } else { String::new() };
    let active_days = active_day_set.len() as u32;
    let daily_avg_tokens = if total == 0 {
        String::new()
    } else if hourly {
        format!("{}/hr", token_label(total / (active_hour_set.len() as u64).max(1)))
    } else {
        format!("{}/day", token_label(total / u64::from(active_days.max(1))))
    };

    // Distinct sessions from sources (deduped across days/models), filtered when a provider is picked.
    let sessions: u64 = summary
        .map(|s| {
            let from_sources: u64 = s
                .sources
                .iter()
                .filter(|src| selected_provider.is_empty() || src.fingerprint.provider.as_str() == selected_provider)
                .map(|src| src.distinct_sessions)
                .sum();
            if from_sources > 0 { from_sources } else { rows.iter().map(|r| r.sessions).max().unwrap_or(0) }
        })
        .unwrap_or(0);

    // Partial/failed source scan notices.
    let mut source_notices: Vec<String> = vec![];
    for src in summary.into_iter().flat_map(|s| &s.sources) {
        if matches!(src.status, UsageSourceStatus::Partial | UsageSourceStatus::Failed) {
            let msg = src
                .message
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| format!("{} history could not be fully scanned.", usage_provider_label(src.fingerprint.provider.as_str())));
            if !source_notices.contains(&msg) {
                source_notices.push(msg);
            }
        }
    }

    let range_label = summary
        .map(|s| {
            if s.since_day == s.until_day {
                short_day_label(&s.since_day).1
            } else {
                format!("{} – {}", short_day_label(&s.since_day).0, short_day_label(&s.until_day).1)
            }
        })
        .unwrap_or_default();

    UsageView {
        loading: model.pages.usage_loading,
        error: model.pages.usage_error.clone(),
        range_label,
        days,
        hourly,
        total_tokens: token_label(total),
        total_tokens_exact: count_label(total as i64),
        total_cost: if summary.is_some() { cost_of(total_usd) } else { String::new() },
        cache_hit_percent,
        cache_hit_label,
        daily_avg_tokens,
        active_days,
        sessions,
        turns: total_turns,
        token_mix,
        selected_provider,
        providers,
        series,
        rows,
        by_model,
        limits: provider_limits(model),
        limits_refreshing: model.pages.usage_limits_refreshing,
        source_notices,
        notice: String::new(),
    }
}

pub fn view(model: &Model) -> ViewModel {
    let env = model.environment.as_ref();
    let reconnect = if model.connection == Connection::Reconnecting && !model.in_flight { model.retry_in } else { 0 };
    let remote = model.active_environment().is_some_and(crate::model::Environment::is_remote);
    let sidebar = sidebar(model);
    let (thread, collected_artifacts) = match thread_page(model) {
        Some((page, collected)) => (Some(page), collected),
        None => (None, Vec::new()),
    };
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
                    companion: crate::companion::resolve(Some(e)),
                })
                .collect(),
            active_environment_id: model.active_env.clone().unwrap_or_default(),
            remote: model.active_environment().is_some_and(crate::model::Environment::is_remote),
            path: if model.tunnel.connected { model.tunnel.path.clone() } else { String::new() },
            rtt_ms: model.tunnel.rtt_ms,
            // Not while this machine's server is signing the app in: there is no code to ask for.
            needs_pairing: model.active_env.is_some() && (model.token.is_none() || model.server_changed) && !model.local_signing_in(),
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
            companion: crate::companion::resolve(model.active_environment()),
            activity: ActivityView {
                working: crate::sidebar::live_threads(model).filter(|thread| shell_status(thread) == ShellStatus::Working).count() as u32,
                attention: sidebar.threads_needing_attention,
                beat: model.companion_beat.0.clone(),
                beat_seq: model.companion_beat.1,
            },
            local_server: local_server(model),
            link_request: link_request(model),
        },
        toast: model.toast.as_ref().map(|t| ToastView {
            id: t.id,
            kind: t.kind.clone(),
            message: t.message.clone(),
            action_label: t.action_label.clone(),
            queued: model.toast_queue.len() as u32,
        }),
        sidebar,
        route: if model.selected_thread.is_some() && model.route == "thread" { "thread".into() } else if model.route.is_empty() { "empty".into() } else { model.route.clone() },
        thread,
        composer: composer(model),
        panel: panel(model, &collected_artifacts),
        palette: model.palette.as_ref().map(|p| PaletteView {
            query: p.query.clone(),
            items: palette_items(model),
            highlighted: p.highlighted as u32,
            mode: if p.mode.is_empty() { "commands".into() } else { p.mode.clone() },
            placeholder: match p.mode.as_str() {
                "files" => "Go to file…".into(),
                "content" => "Search in project…".into(),
                _ => "Type a command or search threads…".into(),
            },
            searching: p.searching,
        }),
        settings: settings(model),
        pull_requests: pull_requests(model),
        issues: crate::issues::view(model),
        usage: usage(model),
        agent_sessions: agent_sessions(model),
        folder_browser: folder_browser(model),
        diagrams: crate::diagrams::view(model),
    }
}

fn sign_in_view(state: Option<&domovoi_model::typed::ProviderAuthState>) -> ProviderSignInView {
    let Some(a) = state else { return ProviderSignInView { phase: "idle".into(), ..Default::default() } };
    let interaction = a.interaction.as_ref().filter(|i| i.kind == "browser");
    ProviderSignInView {
        phase: a.phase.clone(),
        message: a.message.clone().unwrap_or_default(),
        url: interaction.and_then(|i| i.url.clone()).or_else(|| a.authorization_url.clone()).filter(|_| a.phase == "waiting").unwrap_or_default(),
        takes_redirect: a.phase == "waiting" && interaction.and_then(|i| i.accepts_callback).unwrap_or(false),
        busy: a.busy(),
    }
}

fn install_view(state: Option<&domovoi_model::typed::ProviderInstallState>) -> ProviderInstallView {
    let Some(i) = state else { return ProviderInstallView { phase: "idle".into(), ..Default::default() } };
    let mb = |b: u64| format!("{:.0} MB", b as f64 / 1_048_576.0);
    let progress = i.total_bytes.filter(|t| *t > 0).map_or(0, |t| (i.downloaded_bytes.min(t) * 1000 / t) as u32);
    let progress_label = match (i.phase.as_str(), i.total_bytes) {
        ("downloading", Some(t)) => format!("{} of {}", mb(i.downloaded_bytes), mb(t)),
        ("downloading", None) => mb(i.downloaded_bytes),
        _ => String::new(),
    };
    let (installed, offered) = (i.installed_version.clone().unwrap_or_default(), i.version.clone().unwrap_or_default());
    ProviderInstallView {
        phase: i.phase.clone(),
        message: i.message.clone().unwrap_or_default(),
        busy: i.busy(),
        progress,
        progress_label,
        update_available: !i.busy() && !installed.is_empty() && !offered.is_empty() && installed != offered,
        installed_version: installed,
        offered_version: offered,
        can_remove: i.can_remove,
    }
}

/// An Antigravity instance's settings for its editor (the only driver with one).
fn provider_config_view(model: &Model, p: &domovoi_model::typed::ServerProvider) -> ProviderConfigView {
    if p.driver.as_str() != "antigravity" {
        return ProviderConfigView::default();
    }
    let id = p.instance_id.as_str();
    let configured = model.settings.as_ref().and_then(|s| s.provider_instances.as_ref()).and_then(|i| i.get(&p.instance_id));
    let c = configured.and_then(|c| c.config.clone()).unwrap_or(Value::Null);
    let text = |k: &str| c[k].as_str().unwrap_or_default().to_owned();
    let built_in = id == "antigravity";
    // The machine has one Google sign-in for agy: added instances use a key or Vertex.
    let methods: &[(&str, &str)] = if built_in {
        &[("oauth-personal", "Google account"), ("oauth-business", "Gemini Enterprise"), ("gemini-api-key", "Gemini API key"), ("agent-platform", "Agent Platform (Vertex AI)")]
    } else {
        &[("gemini-api-key", "Gemini API key"), ("agent-platform", "Agent Platform (Vertex AI)")]
    };
    let method = c["authMethod"].as_str().map_or_else(|| if built_in { "oauth-personal" } else { "gemini-api-key" }.to_owned(), str::to_owned);
    ProviderConfigView {
        editable: true,
        built_in,
        display_name: configured.and_then(|c| c.display_name.clone()).unwrap_or_default(),
        auth_method: method,
        auth_methods: methods.iter().map(|(v, l)| ChoiceView { value: (*v).into(), label: (*l).into() }).collect(),
        has_api_key: c["apiKey"].as_str().is_some_and(|k| !k.is_empty()),
        gcp_project: text("gcpProject"),
        gcp_location: text("gcpLocation"),
        binary_path: text("binaryPath"),
    }
}

/// The agent a scan found sessions of, as named to the user. Sessions are
/// imported only from Claude Code, Codex and Pi, and a Claude instance can
/// have any id, so anything else unknown stays Claude Code.
pub fn source_label(source: &str) -> &'static str {
    match source {
        s if s.starts_with("codex") => "Codex",
        s if s.starts_with("pi") => "Pi",
        s if s.starts_with("antigravity") => "Antigravity",
        s if s.starts_with("opencode") => "OpenCode",
        _ => "Claude Code",
    }
}

fn agent_sessions(model: &Model) -> AgentSessionsView {
    let st = &model.sessions;
    let two_weeks_ago = crate::app::iso_from_millis(model.shown_ms - 14 * 24 * 60 * 60 * 1000);
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
                detail: match age_label(model.shown_ms, last).as_str() {
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
        is_project: model.projects.iter().any(|p| crate::tools::same_path(&p.workspace_root, &b.path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_identity_matches_web() {
        assert_eq!(project_identity("my cool app"), ("MA".into(), "green"));
        assert_eq!(project_identity("api-v2"), ("AV".into(), "rose"));
        assert_eq!(project_identity("api2"), ("A2".into(), "sky"));
        assert_eq!(project_identity("Project"), ("PT".into(), "fuchsia"));
        assert_eq!(project_identity(""), ("PR".into(), "fuchsia"));
        assert_eq!(monogram("🚀"), "PR");
        assert_eq!(monogram("東京 工具"), "東工");
        assert_eq!(monogram("  ＡＰＩ２  "), "A2");
        assert_eq!(monogram("ß"), "SS");
        assert_eq!(monogram("\u{05b0}"), "PR");
    }

    #[test]
    fn labels() {
        assert_eq!(monogram("my cool app"), "MA");
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
