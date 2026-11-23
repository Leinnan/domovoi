//! What agents are told about Domovoi's own tools (after t3code's
//! `orchestrationInstructions.ts`), given only while the tools are on.
//!
//! Each section stays under 4,000 bytes (Codex cuts longer context entries)
//! and names only tools `crate::mcp` serves. How each provider gets them:
//! Claude as `--append-system-prompt`, Codex as `turn/start`
//! `additionalContext` entries, Pi through its extension's system prompt,
//! OpenCode as the session's system text.

/// The sections, in order.
pub const SECTIONS: [&str; 2] = [
    "## T3 Code orchestration

Ordinary coding tasks need no T3 tools.

- Subagents: use your native tool when the child uses your provider, a model it offers and no settings beyond yours (Claude `Agent`/`Task` in the foreground; Codex `spawn_agent`, then wait). Use `delegate_task` for another provider's model, settings the user asked for, a separate checkout (`isolation:\"worktree\"`), or a child that works in the background.
- Call `orchestrator_capabilities` once, before the first `delegate_task` to another provider or model. Settings go in `target.options`. A provider or model that isn't available: say so. Never switch unasked.
- Don't poll a running delegate (`task_status` loops, `sleep`, watchers): a message reaches this thread when it finishes, and `delegate_task` with `mode:\"wait\"` waits for it. Stop one you replace or no longer need with `task_cancel`.
- As a subagent or delegated child, do the work yourself. Start no subagents unless your task says to.",
    "## T3 Code threads, worktrees, schedules and artifacts

- `t3_thread_launch` and `create_threads` make new top-level threads, only when the user asks for a new thread. \"Subagent\", \"delegate\" and \"parallel\" never mean one. Without `workspaceStrategy` a new thread works in the project's folder, not your worktree.
- Given a thread id, `t3_thread_read` it before you answer about it. Asked to settle, snooze or archive a thread (this one too): `t3_thread_organize`.
- To move this thread into a git worktree of its own: `t3_worktree_status`, then `t3_worktree_handoff`, then end your turn; the next turn runs in the worktree with this conversation.
- Asked only to schedule: `schedule_task`, without running its prompt now. A webhook task (`{\"type\":\"webhook\"}`) runs on each request to its URL; its prompt can use `{{body.path}}`, `{{headers.name}}` and `{{query.name}}`.
- For standalone visual or interactive outputs (HTML dashboards, React JSX/TSX components, SVG graphics, Mermaid diagrams, or long Markdown reports), publish them with `t3_artifact_create` and iterate in place with `t3_artifact_update` instead of pasting large raw HTML/JSX/SVG blocks into chat.
- T3 tools may load late. Try a name once before you call it absent.",
];

/// The MCP server's own `instructions`.
pub const MCP_SUMMARY: &str = "Tools of the T3 Code server you run in: read, message and organize threads of this project, delegate tasks to child agents on any provider, move this thread into a worktree, and schedule tasks.";

/// All of it as one text, for providers that take a system prompt.
pub fn system_prompt() -> String {
    SECTIONS.join("\n\n")
}

/// Use the provider's native question mechanism on interactive turns.
pub fn question_guidance(tool: &str, mode: Option<&str>) -> String {
    let when = match mode {
        Some("plan") => "Ask about unresolved preferences or tradeoffs that materially change the plan.",
        Some(_) => "Ask when an unresolved decision materially changes the outcome or blocks safe progress.",
        None => "In Plan mode, ask about unresolved preferences or tradeoffs that materially change the plan. In other modes, ask when an unresolved decision materially changes the outcome or blocks safe progress.",
    };
    format!("Inspect the available context first. {when} Use the `{tool}` tool for these questions. Ask only what will change the work and include a recommended choice when useful. Proceed on clear requests and routine or readily inferable decisions. If the question tool is unavailable, ask in chat and wait for the reply.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_fit_codex_and_name_only_tools_the_server_has() {
        let tools: Vec<String> = crate::mcp::tools().iter().filter_map(|t| t["name"].as_str().map(str::to_owned)).collect();
        for section in SECTIONS {
            assert!(section.len() < 4_000, "{} bytes", section.len());
            // Every `snake_case` name in backticks that looks like a tool is one.
            for name in section.split('`').skip(1).step_by(2) {
                let looks_like_tool = name.contains('_') && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !["spawn_agent"].contains(&name);
                if looks_like_tool {
                    assert!(tools.iter().any(|t| t == name), "the instructions name {name}, which no tool is");
                }
            }
        }
    }
}
