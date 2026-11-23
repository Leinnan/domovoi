//! What a thread's turn is doing, in a line or two: for surfaces outside the
//! app that show a turn while it runs (the iPhone's Live Activity, which the
//! server keeps current with pushes).

use super::ids::RunId;
use super::plan::PlanStepStatus;
use super::turn_item::{AssistantMessagePhase, TurnItem, TurnItemStatus};
use serde_json::Value;

/// The most of an action (a command, a path) shown.
pub const ACTION_CHARS: usize = 80;
/// The most of a task's text or a turn's answer shown.
pub const SUMMARY_CHARS: usize = 160;

/// A turn's progress, as a glance shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveProgress {
    /// What the latest step is: `command`, `file_change`, `search`, `tool` or `subagent`.
    pub action_kind: Option<&'static str>,
    /// What it works on: the command, the file, the tool and its target.
    pub action: Option<String>,
    /// The turn's to-do list, if it keeps one.
    pub tasks: Option<TaskCount>,
    /// The start of the turn's answer, once it has one.
    pub summary: Option<String>,
}

/// How far a to-do list has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCount {
    pub done: u32,
    pub total: u32,
    /// The step under way (or the first one left), empty if none.
    pub current: String,
}

/// The progress of the run `run_id` (every item's, when `None`) from a
/// thread's turn items in any order.
pub fn live_progress<'a>(items: impl IntoIterator<Item = &'a TurnItem>, run_id: Option<&RunId>) -> LiveProgress {
    let mut items: Vec<&TurnItem> = items.into_iter().filter(|item| run_id.is_none_or(|run| item.run_id() == Some(run))).collect();
    items.sort_by(|a, b| (a.ordinal(), a.id()).cmp(&(b.ordinal(), b.id())));
    // The step under way, or else the latest one: a glance between two steps
    // still says what the turn was last doing.
    let steps = || items.iter().rev().filter_map(|item| step(item));
    let running = items.iter().rev().filter(|item| *item.status() == TurnItemStatus::Running).find_map(|item| step(item));
    let (action_kind, action) = running.or_else(|| steps().next()).map_or((None, None), |(kind, text)| (Some(kind), text));
    LiveProgress { action_kind, action, tasks: items.iter().rev().find_map(|item| tasks(item)), summary: items.iter().rev().find_map(|item| answer(item)) }
}

/// A step's kind and what it works on.
fn step(item: &TurnItem) -> Option<(&'static str, Option<String>)> {
    let title = || item.title().and_then(|t| line(t, ACTION_CHARS));
    Some(match item {
        TurnItem::CommandExecution(c) => ("command", line(&c.input, ACTION_CHARS).or_else(title)),
        TurnItem::FileChange(f) => {
            let path = f.changes.as_ref().and_then(|changes| changes.first()).map_or(f.file_name.as_str(), |first| first.path.as_str());
            ("file_change", path_tail(path).or_else(title))
        }
        TurnItem::FileSearch(s) => ("search", s.pattern.as_deref().and_then(|p| line(p, ACTION_CHARS)).or_else(title)),
        TurnItem::WebSearch(s) => ("search", s.patterns.as_ref().and_then(|p| p.first()).and_then(|p| line(p, ACTION_CHARS)).or_else(title)),
        TurnItem::DynamicTool(t) if t.tool_name.as_deref() == Some("TodoWrite") => return None,
        TurnItem::DynamicTool(t) => {
            let name = t.tool_name.as_deref().map(tool_name).filter(|n| !n.is_empty());
            let target = t.input.as_ref().and_then(tool_target);
            let text = match (name, target) {
                (Some(name), Some(target)) => line(&format!("{name} {target}"), ACTION_CHARS),
                (Some(name), None) => line(name, ACTION_CHARS),
                (None, target) => target.or_else(title),
            };
            ("tool", text)
        }
        TurnItem::Subagent(s) => ("subagent", s.progress.as_deref().and_then(|p| line(p, ACTION_CHARS)).or_else(title)),
        _ => return None,
    })
}

/// `mcp__server__tool` is `tool`; other names stay as they are.
fn tool_name(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// What a tool's arguments say it works on, if they name one thing.
fn tool_target(input: &Value) -> Option<String> {
    ["file_path", "path", "command", "pattern", "url", "query"]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .and_then(|target| if target.contains('/') && !target.contains(' ') { path_tail(target) } else { line(target, ACTION_CHARS) })
}

/// A to-do list's count: a `todo_list` item, or Claude's `TodoWrite` tool,
/// whose input is the list.
fn tasks(item: &TurnItem) -> Option<TaskCount> {
    // (done, under way, text)
    let steps: Vec<(bool, bool, String)> = match item {
        TurnItem::TodoList(list) => list
            .steps
            .iter()
            .map(|s| {
                let under_way = matches!(&s.status, PlanStepStatus::Running) || matches!(&s.status, PlanStepStatus::Other(o) if &**o == "in_progress");
                (s.status == PlanStepStatus::Completed, under_way, s.text.clone())
            })
            .collect(),
        TurnItem::DynamicTool(t) if t.tool_name.as_deref() == Some("TodoWrite") => t
            .input
            .as_ref()
            .and_then(|input| input.get("todos"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|todo| {
                let text = |key: &str| todo.get(key).and_then(Value::as_str);
                let status = text("status").unwrap_or_default();
                (status == "completed", status == "in_progress", text("content").or(text("activeForm")).unwrap_or_default().to_owned())
            })
            .collect(),
        _ => return None,
    };
    if steps.is_empty() {
        return None;
    }
    let done = steps.iter().filter(|s| s.0).count() as u32;
    let current = steps.iter().find(|s| s.1).or_else(|| steps.iter().find(|s| !s.0)).and_then(|s| line(&s.2, SUMMARY_CHARS)).unwrap_or_default();
    Some(TaskCount { done, total: steps.len() as u32, current })
}

/// The first line of an answer the agent gave (not a remark on the way).
fn answer(item: &TurnItem) -> Option<String> {
    let TurnItem::AssistantMessage(message) = item else { return None };
    if matches!(message.phase, Some(AssistantMessagePhase::Commentary)) {
        return None;
    }
    message.text.lines().map(|l| l.trim().trim_start_matches(['#', '*', '-', '>', ' '])).find(|l| !l.is_empty()).and_then(|l| line(l, SUMMARY_CHARS))
}

/// The first non-empty line of `text`, at most `max` characters (cut with `…`).
pub fn line(text: &str, max: usize) -> Option<String> {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(clip(first, max))
}

/// `text` at most `max` characters, cut on a character boundary with `…`.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// A path at most [`ACTION_CHARS`] characters, keeping its end (the file's name).
fn path_tail(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let count = path.chars().count();
    if count <= ACTION_CHARS {
        return Some(path.to_owned());
    }
    Some(format!("…{}", path.chars().skip(count - (ACTION_CHARS - 1)).collect::<String>()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(ordinal: u64, run: &str, status: &str, body: Value) -> TurnItem {
        let mut v = json!({
            "id": format!("i{ordinal}"), "threadId": "t", "runId": run, "nodeId": null, "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal,
            "status": status, "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z",
        });
        for (k, val) in body.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn run(id: &str) -> RunId {
        serde_json::from_value(json!(id)).unwrap()
    }

    #[test]
    fn the_running_step_comes_first_then_the_latest() {
        let items = [
            item(1, "r1", "completed", json!({ "type": "command_execution", "input": "cargo build\nmore" })),
            item(2, "r1", "running", json!({ "type": "command_execution", "input": "cargo test" })),
            item(3, "r1", "completed", json!({ "type": "file_change", "fileName": "src/a.rs" })),
            item(4, "r2", "running", json!({ "type": "command_execution", "input": "other run" })),
        ];
        let p = live_progress(&items, Some(&run("r1")));
        assert_eq!((p.action_kind, p.action.as_deref()), (Some("command"), Some("cargo test")));
        let done = [items[0].clone(), items[2].clone()];
        let p = live_progress(&done, Some(&run("r1")));
        assert_eq!((p.action_kind, p.action.as_deref()), (Some("file_change"), Some("src/a.rs")), "the latest step, finished");
        assert_eq!(live_progress(&done, Some(&run("r1"))).tasks, None);
    }

    #[test]
    fn tools_name_what_they_work_on() {
        let read = item(1, "r", "running", json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "/Users/me/app/src/main.rs" } }));
        assert_eq!(live_progress([&read], None).action.as_deref(), Some("Read /Users/me/app/src/main.rs"));
        let mcp = item(1, "r", "running", json!({ "type": "dynamic_tool", "toolName": "mcp__github__search_issues", "input": { "query": "flaky test" } }));
        assert_eq!(live_progress([&mcp], None).action.as_deref(), Some("search_issues flaky test"));
        let long = format!("/{}/file.rs", "dir/".repeat(40));
        let edit = item(1, "r", "running", json!({ "type": "file_change", "fileName": long }));
        let shown = live_progress([&edit], None).action.unwrap();
        assert!(shown.starts_with('…') && shown.ends_with("/file.rs") && shown.chars().count() == ACTION_CHARS, "{shown}");
    }

    #[test]
    fn tasks_come_from_todo_lists_and_claudes_todo_tool() {
        let list = item(1, "r", "completed", json!({ "type": "todo_list", "planId": "p", "steps": [
            { "id": "1", "text": "Read the code", "status": "completed" },
            { "id": "2", "text": "Fix the bug", "status": "running" },
            { "id": "3", "text": "Run the tests", "status": "pending" },
        ] }));
        assert_eq!(live_progress([&list], None).tasks, Some(TaskCount { done: 1, total: 3, current: "Fix the bug".into() }));
        let todo = item(2, "r", "completed", json!({ "type": "dynamic_tool", "toolName": "TodoWrite", "input": { "todos": [
            { "content": "Write it", "status": "completed", "activeForm": "Writing it" },
            { "content": "Ship it", "status": "pending", "activeForm": "Shipping it" },
        ] } }));
        let p = live_progress([&list, &todo], None);
        assert_eq!(p.tasks, Some(TaskCount { done: 1, total: 2, current: "Ship it".into() }), "the latest list");
        assert_eq!(p.action_kind, None, "the to-do tool is not a step");
    }

    #[test]
    fn the_summary_is_the_answer_not_a_remark() {
        let remark = item(1, "r", "completed", json!({ "type": "assistant_message", "phase": "commentary", "messageId": "m1", "text": "Let me look.", "streaming": false }));
        assert_eq!(live_progress([&remark], None).summary, None);
        let answer = item(2, "r", "completed", json!({ "type": "assistant_message", "phase": "final_answer", "messageId": "m2", "text": "\n## Fixed the three failing tests\n\nDetails…", "streaming": false }));
        assert_eq!(live_progress([&remark, &answer], None).summary.as_deref(), Some("Fixed the three failing tests"));
        assert_eq!(clip(&"ż".repeat(200), 10).chars().count(), 10);
    }
}
