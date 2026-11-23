//! What a thread's "Allow for Thread" answers allowed, kept past the
//! provider session that asked (Domovoi's own: t3code keeps them only for
//! the session). A thread that runs every day (a scheduled task) is asked
//! once, not on every run.
//!
//! Only Claude's allow rules are kept: they go to each new session of the
//! thread as `--allowedTools`. Other providers keep their own grants.

#![expect(clippy::disallowed_types, reason = "Claude's permission updates, as its CLI sends and reads them")]

use crate::store::Store;
use rusqlite::params;
use serde_json::Value;

pub const CLAUDE: &str = "claudeAgent";

/// The `--allowedTools` entries a Claude permission update allows:
/// `Tool` or `Tool(content)`. A rule with a comma would split the list, and
/// stays for its session only.
pub fn claude_rules(updates: &[Value]) -> Vec<String> {
    updates
        .iter()
        .filter(|u| u["type"] == "addRules" && u["behavior"] == "allow")
        .flat_map(|u| u["rules"].as_array().cloned().unwrap_or_default())
        .filter_map(|rule| {
            let tool = rule["toolName"].as_str().filter(|t| !t.is_empty())?;
            Some(match rule["ruleContent"].as_str().filter(|c| !c.is_empty()) {
                Some(content) => format!("{tool}({content})"),
                None => tool.to_owned(),
            })
        })
        .filter(|rule| !rule.contains(','))
        .collect()
}

/// Whether one of `rules` (as [`claude_rules`] makes them) allows Claude's
/// `can_use_tool` `request`: a bare `Tool` rule allows the tool, `Bash(cmd)`
/// only that exact command. Other rules with content are Claude's to read.
pub fn allows(rules: &[String], request: &Value) -> bool {
    let Some(tool) = request["tool_name"].as_str().filter(|t| !t.is_empty()) else { return false };
    let command = request["input"]["command"].as_str();
    rules.iter().any(|rule| match rule.split_once('(') {
        None => rule == tool,
        Some((name, content)) => name == "Bash" && tool == "Bash" && content.strip_suffix(')').is_some_and(|c| Some(c) == command),
    })
}

/// Keeps `rules` for `thread_id`'s later sessions of `provider`.
pub fn remember(store: &Store, thread_id: &str, provider: &str, rules: &[String]) {
    if rules.is_empty() {
        return;
    }
    let now = domovoi_contracts::iso_now();
    let kept = store.db().with(|c| {
        for rule in rules {
            c.execute("INSERT OR IGNORE INTO thread_permission_rules VALUES (?1, ?2, ?3, ?4)", params![thread_id, provider, rule, now])?;
        }
        Ok(())
    });
    if let Err(e) = kept {
        tracing::warn!(%thread_id, "could not keep a thread's allowed tools: {e}");
    }
}

/// What `thread_id`'s earlier answers allowed `provider`, oldest first.
pub fn rules(store: &Store, thread_id: &str, provider: &str) -> Vec<String> {
    store
        .db()
        .read(|c| {
            let mut st = c.prepare("SELECT rule FROM thread_permission_rules WHERE thread_id = ?1 AND provider = ?2 ORDER BY added_at, rule")?;
            st.query_map(params![thread_id, provider], |r| r.get::<_, String>(0))?.collect()
        })
        .unwrap_or_default()
}

/// Forgets everything `thread_id`'s answers allowed; returns how many rules went.
pub fn forget(store: &Store, thread_id: &str) -> usize {
    store.db().with(|c| c.execute("DELETE FROM thread_permission_rules WHERE thread_id = ?1", params![thread_id])).unwrap_or(0)
}

/// The store, once the server runs (a provider session reads and writes through it).
pub fn store() -> Option<&'static Store> {
    crate::state::STATE.get().map(|state| &state.store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::json;

    #[test]
    fn allow_rules_are_kept_per_thread_and_provider_until_forgotten() {
        let updates = [
            json!({ "type": "addRules", "rules": [{ "toolName": "WebFetch", "ruleContent": "domain:allegro.pl" }, { "toolName": "WebSearch" }], "behavior": "allow", "destination": "session" }),
            json!({ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "echo a,b" }], "behavior": "allow", "destination": "session" }),
            json!({ "type": "addRules", "rules": [{ "toolName": "Bash" }], "behavior": "deny", "destination": "session" }),
            json!({ "type": "setMode", "mode": "acceptEdits", "destination": "session" }),
        ];
        let rules = claude_rules(&updates);
        assert_eq!(rules, ["WebFetch(domain:allegro.pl)", "WebSearch"], "allow rules only, none with a comma");

        let store = Store::new(Db::open_in_memory().unwrap());
        remember(&store, "t1", CLAUDE, &rules);
        remember(&store, "t1", CLAUDE, &["WebSearch".to_owned()]);
        assert_eq!(super::rules(&store, "t1", CLAUDE), ["WebFetch(domain:allegro.pl)", "WebSearch"], "each once");
        assert!(super::rules(&store, "t2", CLAUDE).is_empty());
        assert!(super::rules(&store, "t1", "codex").is_empty());
        assert_eq!(forget(&store, "t1"), 2);
        assert!(super::rules(&store, "t1", CLAUDE).is_empty());
    }

    #[test]
    fn a_kept_rule_allows_its_tool_or_its_exact_command() {
        let rules = ["Edit".to_owned(), "Bash(npm test)".to_owned(), "WebFetch(domain:allegro.pl)".to_owned()];
        let request = |tool: &str, input: Value| json!({ "subtype": "can_use_tool", "tool_name": tool, "input": input });
        assert!(allows(&rules, &request("Edit", json!({ "file_path": "/a.swift" }))));
        assert!(!allows(&rules, &request("Write", json!({ "file_path": "/a.swift" }))), "another tool");
        assert!(allows(&rules, &request("Bash", json!({ "command": "npm test" }))));
        assert!(!allows(&rules, &request("Bash", json!({ "command": "npm test && rm -rf /" }))), "only that command");
        assert!(!allows(&rules, &request("WebFetch", json!({ "url": "https://allegro.pl" }))), "left to Claude");
        assert!(!allows(&rules, &json!({ "subtype": "can_use_tool" })));
        assert!(!allows(&[], &request("Edit", json!({}))));
    }
}
