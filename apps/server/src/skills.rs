//! Provider skills (t3code `ClaudeSkills`, `CodexProvider` skills and
//! `ClaudeSkillDispatch`).
//!
//! Claude: the skills its last session loaded, as its `system/init` names
//! them (Claude's own rules decide which: user, project and plugin skills),
//! described by its `initialize` command list. Kept in a file so a restarted
//! server offers them before a new session.
//!
//! Codex: the `SKILL.md` folders under `<CODEX_HOME>/skills` (its own
//! `.system` ones too), described by their front matter. Codex reads a
//! `$name` mention itself.
//!
//! A `$name` mention of a Claude skill becomes `/name`, which Claude expands
//! only at the start of a text block: the last mention starts the message's
//! last block, with the text after it.

#![expect(clippy::disallowed_types, reason = "Claude's system/init message and initialize answer, read as Claude sends them")]

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use domovoi_model::typed::ServerProviderSkill;

static CLAUDE_SKILLS: RwLock<Vec<ServerProviderSkill>> = RwLock::new(Vec::new());
/// Command descriptions from Claude's last `initialize` answer.
static DESCRIPTIONS: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);
static FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Loads the skills remembered in `dir` and remembers new ones there.
pub fn init(dir: &Path) {
    let file = dir.join("claude-skills.json");
    if let Some(list) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<Vec<ServerProviderSkill>>(&t).ok()) {
        *CLAUDE_SKILLS.write().unwrap_or_else(|e| e.into_inner()) = list;
    }
    let _ = FILE.set(file);
}

/// The descriptions in an `initialize` answer's `commands`.
pub fn remember_descriptions(initialized: &Value) {
    let commands = initialized["response"]["commands"].as_array().or_else(|| initialized["commands"].as_array());
    let Some(commands) = commands else { return };
    let map: HashMap<String, String> = commands
        .iter()
        .filter_map(|c| Some((c["name"].as_str()?.trim_start_matches('/').to_owned(), c["description"].as_str()?.trim().to_owned())))
        .filter(|(_, d)| !d.is_empty())
        .collect();
    *DESCRIPTIONS.write().unwrap_or_else(|e| e.into_inner()) = Some(map);
}

/// A command's description, from Claude's last `initialize`.
pub fn description(name: &str) -> Option<String> {
    DESCRIPTIONS.read().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(name).cloned()
}

/// Remembers the skills a `system/init` names; whether they changed.
pub fn remember_claude(init: &Value, cwd: &str) -> bool {
    let Some(names) = init["skills"].as_array() else { return false };
    let mut seen = std::collections::HashSet::new();
    let list: Vec<ServerProviderSkill> = names
        .iter()
        .filter_map(Value::as_str)
        .map(|n| n.trim().trim_start_matches('/'))
        .filter(|n| !n.is_empty() && seen.insert(n.to_owned()))
        .map(|name| claude_skill(name, cwd))
        .collect();
    {
        let mut current = CLAUDE_SKILLS.write().unwrap_or_else(|e| e.into_inner());
        if *current == list {
            return false;
        }
        *current = list.clone();
    }
    if let Some(file) = FILE.get() {
        let _ = std::fs::write(file, serde_json::to_string(&list).unwrap_or_default());
    }
    true
}

/// Where a Claude skill lives, as well as Domovoi can tell: a plain name in
/// the user's or the project's `skills`, a `plugin:name` in a plugin.
fn claude_skill(name: &str, cwd: &str) -> ServerProviderSkill {
    let user = claude_config_dir().join("skills").join(name).join("SKILL.md");
    let project = Path::new(cwd).join(".claude").join("skills").join(name).join("SKILL.md");
    let (path, scope) = if name.contains(':') {
        (format!("plugin:{name}"), "plugin")
    } else if user.is_file() {
        (user.to_string_lossy().into_owned(), "user")
    } else if project.is_file() {
        (project.to_string_lossy().into_owned(), "project")
    } else {
        (format!("claude:{name}"), "user")
    };
    let front = std::fs::read_to_string(&path).ok().map(|t| front_matter(&t)).unwrap_or_default();
    let description = front.get("description").cloned().or_else(|| description(name));
    ServerProviderSkill {
        name: name.to_owned(),
        description,
        path,
        scope: Some(scope.to_owned()),
        enabled: true,
        display_name: None,
        short_description: None,
        user_invocation_only: (front.get("disable-model-invocation").map(String::as_str) == Some("true")).then_some(true),
        user_invocable: (front.get("user-invocable").map(String::as_str) == Some("false")).then_some(false),
    }
}

pub fn claude() -> Vec<ServerProviderSkill> {
    CLAUDE_SKILLS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn agent_home() -> PathBuf {
    std::env::var_os("DOMOVOI_AGENT_SESSIONS_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(crate::util::home_dir)
}

fn isolated() -> bool {
    std::env::var_os("DOMOVOI_AGENT_SESSIONS_HOME").is_some_and(|v| !v.is_empty())
}

fn claude_config_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty() && !isolated()) {
        Some(dir) => PathBuf::from(dir),
        None => agent_home().join(".claude"),
    }
}

fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty() && !isolated()) {
        Some(dir) => PathBuf::from(dir),
        None => agent_home().join(".codex"),
    }
}

/// `key: value` lines between the opening `---` and the next one (quotes dropped).
pub fn front_matter(text: &str) -> HashMap<String, String> {
    let mut lines = text.lines();
    let mut out = HashMap::new();
    if lines.next().map(str::trim) != Some("---") {
        return out;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if !v.is_empty() && v != "|" && v != ">" {
                out.insert(k.trim().to_owned(), v.to_owned());
            }
        }
    }
    out
}

/// Codex's skills: each folder under `skills` (and `skills/.system`) with a `SKILL.md`.
pub fn codex() -> Vec<ServerProviderSkill> {
    let root = codex_home().join("skills");
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    for (dir, scope) in [(root.clone(), "user"), (root.join(".system"), "system")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
        entries.sort();
        for folder in entries {
            let file = folder.join("SKILL.md");
            let front = std::fs::read_to_string(&file).map(|t| front_matter(&t)).unwrap_or_default();
            let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name = front.get("name").cloned().unwrap_or(folder_name);
            if name.is_empty() || !seen.insert(name.clone()) {
                continue;
            }
            out.push(ServerProviderSkill {
                name,
                description: front.get("description").cloned(),
                path: file.to_string_lossy().into_owned(),
                scope: Some(scope.to_owned()),
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: None,
                user_invocable: None,
            });
        }
    }
    out
}

/// A `$name` mention (t3code `SKILL_MENTION_PATTERN`): after a space or the
/// start, a currency sign, then a name of letters, digits, `:`, `_` and `-`
/// with at least one letter, not a price, ending at a space or the end.
fn mentions(prompt: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<(usize, char)> = prompt.char_indices().collect();
    let mut out = vec![];
    for (i, &(at, c)) in chars.iter().enumerate() {
        let after_space = i == 0 || chars[i - 1].1.is_whitespace();
        if !after_space || !is_currency(c) {
            continue;
        }
        let name_start = at + c.len_utf8();
        let name: String = prompt[name_start..].chars().take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-')).collect();
        let end = name_start + name.len();
        let ends_well = prompt[end..].chars().next().is_none_or(char::is_whitespace);
        if name.is_empty() || !ends_well || !name.chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric()) || !name.chars().any(|ch| ch.is_ascii_alphabetic()) || price(&name) {
            continue;
        }
        out.push((at, end, name));
    }
    out
}

/// `5`, `10k`, `2.5`-like amounts are prices, not skills.
fn price(name: &str) -> bool {
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return false;
    }
    let rest = &name[digits.len()..];
    rest.is_empty() || matches!(rest.to_ascii_lowercase().as_str(), "k" | "m" | "b" | "t") || rest.strip_prefix(['e', 'E']).is_some_and(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_digit()))
}

fn is_currency(c: char) -> bool {
    matches!(c, '$' | '¢' | '£' | '¤' | '¥' | '€' | '₹' | '₽' | '₩' | '₪' | '₫' | '₱')
}

/// The text blocks a Claude message is sent as, when it mentions a skill
/// Claude has; `None` otherwise (t3code `planClaudeSkillDispatch`).
pub fn claude_dispatch(prompt: &str, skills: &[ServerProviderSkill]) -> Option<Vec<String>> {
    let found: Vec<(usize, usize, &ServerProviderSkill)> =
        mentions(prompt).into_iter().filter_map(|(s, e, name)| skills.iter().find(|k| k.name == name).map(|k| (s, e, k))).collect();
    let &(last_start, last_end, last) = found.last()?;
    let mut annotated = std::collections::HashSet::from([last.name.as_str()]);
    let mut inline = String::new();
    let mut cursor = 0;
    for (start, end, skill) in &found {
        inline.push_str(&prompt[cursor..*start]);
        inline.push('/');
        inline.push_str(&skill.name);
        if skill.user_invocation_only == Some(true) && annotated.insert(skill.name.as_str()) {
            inline.push_str(&format!(" [The user invoked this skill. Follow the instructions at: {}]", skill.path));
        }
        cursor = *end;
    }
    inline.push_str(&prompt[cursor..]);
    let command = format!("/{}{}", last.name, &prompt[last_end..]).trim_end().to_owned();
    let mut blocks = vec![];
    if !prompt[..last_start].trim().is_empty() {
        blocks.push(inline.trim_end().to_owned());
    }
    blocks.push(command);
    Some(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, user_only: bool) -> ServerProviderSkill {
        ServerProviderSkill { name: name.into(), description: None, path: format!("/s/{name}/SKILL.md"), scope: None, enabled: true, display_name: None,
            short_description: None, user_invocation_only: user_only.then_some(true), user_invocable: None }
    }

    #[test]
    fn a_skill_mention_becomes_a_command_claude_expands() {
        let skills = [skill("review", false), skill("plugin:deploy", true)];
        assert_eq!(claude_dispatch("$review the diff", &skills), Some(vec!["/review the diff".to_owned()]));
        assert_eq!(
            claude_dispatch("first $plugin:deploy then $review it", &skills),
            Some(vec![
                "first /plugin:deploy [The user invoked this skill. Follow the instructions at: /s/plugin:deploy/SKILL.md] then /review it".to_owned(),
                "/review it".to_owned()
            ])
        );
        assert_eq!(claude_dispatch("costs $5 or $10k", &skills), None, "prices are no skills");
        assert_eq!(claude_dispatch("use $unknown", &skills), None);
        assert_eq!(claude_dispatch("a$review", &skills), None, "only after a space");
    }

    #[test]
    fn front_matter_is_read() {
        let f = front_matter("---\nname: \"pdf\"\ndescription: Read PDFs\nmetadata:\n  x: y\ndisable-model-invocation: true\n---\n# body\nname: no");
        assert_eq!((f["name"].as_str(), f["description"].as_str(), f["disable-model-invocation"].as_str()), ("pdf", "Read PDFs", "true"));
        assert!(!f.contains_key("x"));
        assert!(front_matter("no front matter").is_empty());
    }

    /// As Claude Code 2.1.319 sends them: `initialize` lists commands with
    /// descriptions, `system/init` names the skills.
    #[test]
    fn claude_skills_come_from_its_init() {
        remember_descriptions(&serde_json::json!({ "subtype": "success", "response": { "commands": [
            { "name": "deep-research", "description": "Deep research harness", "argumentHint": "" },
            { "name": "small-business:ad-manager", "description": "(Small Business) The ads consultant" }] } }));
        let init = serde_json::json!({ "type": "system", "subtype": "init", "skills": ["deep-research", "small-business:ad-manager", "deep-research"] });
        remember_claude(&init, "/nowhere");
        let skills = claude();
        assert_eq!(skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["deep-research", "small-business:ad-manager"]);
        assert_eq!(skills[0].description.as_deref(), Some("Deep research harness"));
        assert_eq!((skills[1].scope.as_deref(), skills[1].path.as_str()), (Some("plugin"), "plugin:small-business:ad-manager"));
        assert!(!remember_claude(&init, "/nowhere"), "the same skills again are no change");
    }
}
