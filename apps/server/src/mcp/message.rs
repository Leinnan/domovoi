//! `send_to_user` and `tool_search` (t3code
//! `toolSearch` toolkit).
//!
//! `send_to_user` shows a Markdown message mid-turn: the tool call itself,
//! recorded in the thread with its input, is what the user reads, so the
//! tool only answers that it was shown. `tool_search` finds this server's
//! tools by name or purpose; every tool is already listed (Domovoi does not
//! hold tools back), so it only searches.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, s, tool};
use serde_json::{Value, json};

const MAX_MESSAGE_CHARS: usize = 4_000;
const MAX_QUERY_CHARS: usize = 500;
const MAX_MATCHES: usize = 20;

pub fn tools() -> Vec<Value> {
    vec![
        tool("send_to_user", "Show the user a Markdown message in this thread, exactly as written, before your turn ends. Use it for content the user must read in full mid-turn, such as a draft, a code block, or what a question you are about to ask refers to. Don't repeat it in your reply.",
            json!({ "message": s("Markdown, shown verbatim (at most 4000 characters).") }), &["message"], true),
        tool("tool_search", "Find Domovoi tools by name or purpose, then call them by name: threads (thread_*), worktrees, delegated tasks, schedules, artifacts, html_render, pull requests, attachments, request_secret. Exact names work too.",
            json!({ "query": s("Names or words, at most 500 characters.") }), &["query"], true),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "send_to_user" => send_to_user(c),
        "tool_search" => search(c),
        _ => return None,
    })
}

fn send_to_user(c: &Call<'_>) -> Result<Value, String> {
    let message = c.args["message"].as_str().filter(|m| !m.trim().is_empty()).ok_or("message is required.")?;
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("invalid_request: message is longer than {MAX_MESSAGE_CHARS} characters."));
    }
    Ok(json!({ "sentMessage": message, "message": "Shown to the user. Don't repeat it." }))
}

/// Tools ranked by how many of the query's words their name or description holds; an exact name first.
pub(crate) fn matches(tools: &[Value], query: &str) -> Vec<Value> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).map(super::current_tool_name).collect();
    let mut scored: Vec<(usize, &Value)> = tools
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?.to_lowercase();
            let text = format!("{name} {}", t["description"].as_str().unwrap_or_default().to_lowercase());
            let exact = words.iter().any(|w| *w == name || w.strip_prefix("select:").is_some_and(|list| list.split(',').any(|n| super::current_tool_name(n) == name)));
            let hits = words.iter().filter(|w| text.contains(**w) || name.contains(&w.replace('-', "_"))).count();
            let score = if exact { usize::MAX } else { hits };
            (score > 0).then_some((score, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(MAX_MATCHES).map(|(_, t)| json!({ "name": t["name"], "description": t["description"] })).collect()
}

fn search(c: &Call<'_>) -> Result<Value, String> {
    let query = c.args["query"].as_str().unwrap_or_default();
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("invalid_request: query is longer than {MAX_QUERY_CHARS} characters."));
    }
    Ok(json!({ "tools": matches(&super::tools(), query) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_search_finds_by_name_and_purpose() {
        let tools = super::super::tools();
        let found = matches(&tools, "request_secret");
        assert_eq!(found[0]["name"], "request_secret");
        let found = matches(&tools, "pull request");
        assert!(found.iter().any(|t| t["name"] == "link_pull_request"), "{found:?}");
        assert!(matches(&tools, "mcp__domovoi__html_render")[0]["name"] == "html_render");
        assert!(matches(&tools, "mcp__t3__html_render")[0]["name"] == "html_render", "as it was named");
        assert!(matches(&tools, "zzzz-nothing").is_empty());
    }
}
