//! What agents are told about Domovoi's own tools (after t3code's
//! `orchestrationInstructions.ts`), given only while the tools are on.
//!
//! Each section stays under 4,000 bytes (Codex cuts longer context entries)
//! and names only tools `crate::mcp` serves. How each provider gets them:
//! Claude as `--append-system-prompt`, Codex as `turn/start`
//! `additionalContext` entries, Pi through its extension's system prompt,
//! OpenCode as the session's `t3-code` instruction entry.

/// The sections, in order.
pub const SECTIONS: [&str; 3] = [
    "## Domovoi orchestration

Ordinary coding tasks need none of Domovoi's tools.

- Subagents: use your native tool when the child uses your provider, a model it offers and no settings beyond yours (Claude `Agent`/`Task` in the foreground; Codex `spawn_agent`, then wait). Use `delegate_task` for another provider's model, settings the user asked for, a separate checkout (`isolation:\"worktree\"`), or a child that works in the background.
- Call `orchestrator_capabilities` once, before the first `delegate_task` to another provider or model. Settings go in `target.options`. A provider or model that isn't available: say so. Never switch unasked.
- Don't poll a running delegate (`task_status` loops, `sleep`, watchers): a message reaches this thread when it finishes, and `delegate_task` with `mode:\"wait\"` waits for it. Stop one you replace or no longer need with `task_cancel`.
- As a subagent or delegated child, do the work yourself. Start no subagents unless your task says to.",
    "## Domovoi threads, worktrees, schedules, secrets and artifacts

- `thread_launch` and `create_threads` make new top-level threads, only when the user asks for a new thread. \"Subagent\", \"delegate\" and \"parallel\" never mean one. Without `workspaceStrategy` a new thread works in the project's folder, not your worktree.
- Given a thread id, `thread_read` it before you answer about it. Asked to settle, snooze or archive a thread (this one too): `thread_organize`.
- To move this thread into a git worktree of its own: `worktree_status`, then `worktree_handoff`, then end your turn; the next turn runs in the worktree with this conversation.
- Asked only to schedule: `schedule_task`, without running its prompt now. A webhook task (`{\"type\":\"webhook\"}`) runs on each request to its URL; its prompt can use `{{body.path}}`, `{{headers.name}}` and `{{query.name}}`.
- For standalone visual or interactive outputs (HTML dashboards, React JSX/TSX components, SVG graphics, Mermaid diagrams, or long Markdown reports), publish them with `artifact_create` and iterate in place with `artifact_update` instead of pasting large raw HTML/JSX/SVG blocks into chat.
- A login, token, PIN, pairing code or sudo password only the user can give: call `request_secret`, not a chat ask or a handoff. Not for access you have. Sudo: if `sudo -n <cmd>` needs a password, use `delivery: \"file\"` and run `sudo -S -p '' <cmd> < \"$file\"`.
- A PR you open or are given for this thread: `link_pull_request`. Its state and checks: `pull_request_read`, not `gh`.
- Domovoi's tools may load late. Try a name once before you call it absent.",
    "## Domovoi visuals and browser

- Answer data with a trend or comparison with a chart and show any visual the user asks for here: pass inline HTML (or, in a full-access thread, an HTML file path) to `html_preview`, then `html_render`. A screenshot on disk or a text description doesn't show it. Never start a server for it. Asked for an image to post, send or use elsewhere: a `.png` file and its path, never `html_render`.
- Never use `preview_*` unasked to verify or show your own edit. Use them when the current request asks for browser work, or names a page or tab. Don't repeat preview calls from an earlier turn. Public pages: web fetch or search unless browser work was asked; then preview tools only, no web fetch. Page tools write only on user request; never use them to show results. Close a tab you opened with `preview_close` before you reply, unless the user asked for it or uses it.
- When browser work was asked, `preview_*` is the browser you share with the user: `preview_status` first, then `preview_open` if no tab can be automated. Prefer snapshot locators over coordinates. Switch browsers only if the tools are absent or `preview_open` says unavailable, the user asks, or two preview calls fail on one step. Then quote the error and switch.",
];

/// The MCP server's own `instructions`.
pub const MCP_SUMMARY: &str = "Tools of the Domovoi server you run in: read, message and organize threads of this project, delegate tasks to child agents on any provider, move this thread into a worktree, schedule tasks, ask the user for a secret, link pull requests, send attachments, preview and show HTML pages, and use the browser you share with the user.";

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
