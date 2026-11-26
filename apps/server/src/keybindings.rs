//! Keybindings:
//! `<userdata>/keybindings.json` holds rules like
//! `{ "key": "mod+shift+k", "command": "...", "when": "a && !b" }`, seeded
//! with t3code's defaults; clients get them compiled to shortcut objects and
//! `when` ASTs, merged over the defaults for commands the file does not bind.

#![expect(clippy::disallowed_types, reason = "the user's keybindings file: an entry is kept as written (an invalid one too) so a save rewrites the file whole")]

use crate::config_failure::Failure;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::path::Path;
use domovoi_model::typed::{
    KeybindingShortcut, KeybindingWhenNode, KeybindingsInvalidEntryIssue, KeybindingsMalformedConfigIssue, ResolvedKeybindingRule,
    ServerConfigIssue, ServerConfigKeybindingsUpdatedPayload, ServerUpsertKeybindingResult,
};

const MAX_KEYBINDINGS: usize = 256;
const MAX_WHEN_DEPTH: usize = 64;

/// A string, or nothing for anything else.
fn text_or_none<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_str().map(str::to_owned))
}

/// One rule as the keybindings file spells it (`KeybindingRule`): a shortcut,
/// what it does, and the condition it applies under. A `when` that is not a
/// string is no condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeybindingRule {
    pub key: String,
    pub command: String,
    #[serde(default, deserialize_with = "text_or_none", skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// One entry of the keybindings file. An entry this build cannot read as a
/// rule (a typo, something a newer build wrote) has no `rule` and is written
/// back unchanged, so that editing another shortcut keeps it; t3code keeps
/// its entries the same way (`{ value, rule }`).
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// The entry as the file has it.
    written: Value,
    rule: Option<KeybindingRule>,
}

impl Entry {
    fn of_file(written: Value) -> Entry {
        let rule = KeybindingRule::deserialize(&written).ok();
        Entry { written, rule }
    }

    fn of_rule(rule: KeybindingRule) -> Entry {
        Entry { written: serde_json::to_value(&rule).expect("a rule serializes"), rule: Some(rule) }
    }
}

/// The keybindings in force and what is wrong with the file, as
/// `ServerConfig` and the answers about keybindings carry them.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub keybindings: Vec<ResolvedKeybindingRule>,
    pub issues: Vec<ServerConfigIssue>,
}

impl From<Resolved> for ServerUpsertKeybindingResult {
    fn from(resolved: Resolved) -> Self {
        ServerUpsertKeybindingResult { keybindings: resolved.keybindings.into(), issues: resolved.issues.into() }
    }
}

impl From<Resolved> for ServerConfigKeybindingsUpdatedPayload {
    fn from(resolved: Resolved) -> Self {
        ServerConfigKeybindingsUpdatedPayload { keybindings: resolved.keybindings.into(), issues: resolved.issues.into() }
    }
}

pub fn default_rules() -> Vec<KeybindingRule> {
    serde_json::from_str(crate::defaults::KEYBINDING_RULES).expect("bundled keybindings are valid JSON")
}

/// `parseKeybindingShortcut`.
pub fn parse_shortcut(value: &str) -> Option<KeybindingShortcut> {
    let lower = value.to_lowercase();
    let mut tokens: Vec<&str> = lower.split('+').map(str::trim).collect();
    let mut trailing = 0;
    while tokens.last() == Some(&"") {
        trailing += 1;
        tokens.pop();
    }
    if trailing > 0 {
        tokens.push("+");
    }
    if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
        return None;
    }
    let (mut meta, mut ctrl, mut shift, mut alt, mut m) = (false, false, false, false, false);
    let mut key: Option<String> = None;
    for t in tokens {
        match t {
            "cmd" | "meta" => meta = true,
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "mod" => m = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(match other {
                    "space" => " ".into(),
                    "esc" => "escape".into(),
                    o => o.into(),
                });
            }
        }
    }
    Some(KeybindingShortcut { key: key?, meta_key: meta, ctrl_key: ctrl, shift_key: shift, alt_key: alt, mod_key: m })
}

/// `encodeShortcut`: the normalized spelling of a shortcut.
fn encode_shortcut(s: &KeybindingShortcut) -> Option<String> {
    let key = s.key.as_str();
    if key.is_empty() || (key != "+" && key.contains('+')) {
        return None;
    }
    let mut parts: Vec<&str> = vec![];
    for (flag, name) in [(s.mod_key, "mod"), (s.meta_key, "meta"), (s.ctrl_key, "ctrl"), (s.alt_key, "alt"), (s.shift_key, "shift")] {
        if flag {
            parts.push(name);
        }
    }
    parts.push(if key == " " { "space" } else { key });
    Some(parts.join("+"))
}

#[derive(Debug, PartialEq)]
enum Tok {
    And,
    Or,
    Not,
    L,
    R,
    Id(String),
}

fn tokenize(expr: &str) -> Option<Vec<Tok>> {
    let b: Vec<char> = expr.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '&' && b.get(i + 1) == Some(&'&') {
            out.push(Tok::And);
            i += 2;
        } else if c == '|' && b.get(i + 1) == Some(&'|') {
            out.push(Tok::Or);
            i += 2;
        } else if c == '!' {
            out.push(Tok::Not);
            i += 1;
        } else if c == '(' {
            out.push(Tok::L);
            i += 1;
        } else if c == ')' {
            out.push(Tok::R);
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], '_' | '.' | '-')) {
                i += 1;
            }
            out.push(Tok::Id(b[start..i].iter().collect()));
        } else {
            return None;
        }
    }
    Some(out)
}

/// `parseKeybindingWhenExpression`: `||` binds looser than `&&`, `!` tightest.
pub fn parse_when(expr: &str) -> Option<KeybindingWhenNode> {
    struct P {
        t: Vec<Tok>,
        i: usize,
    }
    impl P {
        fn primary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
            if depth > MAX_WHEN_DEPTH {
                return None;
            }
            match self.t.get(self.i)? {
                Tok::Id(name) => {
                    let v = KeybindingWhenNode::Identifier { name: name.clone() };
                    self.i += 1;
                    Some(v)
                }
                Tok::L => {
                    self.i += 1;
                    let e = self.or(depth + 1)?;
                    if self.t.get(self.i) != Some(&Tok::R) {
                        return None;
                    }
                    self.i += 1;
                    Some(e)
                }
                _ => None,
            }
        }
        fn unary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
            let mut nots = 0;
            while self.t.get(self.i) == Some(&Tok::Not) {
                self.i += 1;
                nots += 1;
                if nots > MAX_WHEN_DEPTH {
                    return None;
                }
            }
            let mut node = self.primary(depth)?;
            for _ in 0..nots {
                node = KeybindingWhenNode::Not { node: Box::new(node) };
            }
            Some(node)
        }
        fn and(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
            let mut left = self.unary(depth)?;
            while self.t.get(self.i) == Some(&Tok::And) {
                self.i += 1;
                let right = self.unary(depth)?;
                left = KeybindingWhenNode::And { left: Box::new(left), right: Box::new(right) };
            }
            Some(left)
        }
        fn or(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
            let mut left = self.and(depth)?;
            while self.t.get(self.i) == Some(&Tok::Or) {
                self.i += 1;
                let right = self.and(depth)?;
                left = KeybindingWhenNode::Or { left: Box::new(left), right: Box::new(right) };
            }
            Some(left)
        }
    }
    let t = tokenize(expr)?;
    if t.is_empty() {
        return None;
    }
    let mut p = P { t, i: 0 };
    let ast = p.or(0)?;
    (p.i == p.t.len()).then_some(ast)
}

fn encode_when(n: &KeybindingWhenNode) -> String {
    match n {
        KeybindingWhenNode::Identifier { name } => name.clone(),
        KeybindingWhenNode::Not { node } => format!("!({})", encode_when(node)),
        KeybindingWhenNode::And { left, right } => format!("({} && {})", encode_when(left), encode_when(right)),
        KeybindingWhenNode::Or { left, right } => format!("({} || {})", encode_when(left), encode_when(right)),
        KeybindingWhenNode::Other(_) => String::new(),
    }
}

/// `compileResolvedKeybindingRule`: invalid rules compile to nothing.
pub fn compile(rule: &KeybindingRule) -> Option<ResolvedKeybindingRule> {
    let shortcut = parse_shortcut(&rule.key)?;
    let when_ast = match &rule.when {
        Some(when) => Some(parse_when(when)?),
        None => None,
    };
    Some(ResolvedKeybindingRule { command: rule.command.as_str().into(), shortcut, when_ast })
}

/// `keybindingRuleIdentity`: rules compare by parsed form, not spelling
/// (command, shortcut, condition).
fn identity(rule: &KeybindingRule) -> (String, String, Option<String>) {
    let key = parse_shortcut(&rule.key).and_then(|s| encode_shortcut(&s)).unwrap_or_else(|| rule.key.clone());
    let when = rule.when.as_ref().map(|w| parse_when(w).map(|a| encode_when(&a)).unwrap_or_else(|| w.clone()));
    (rule.command.clone(), key, when)
}

/// The entries of the file, the defaults when it is missing.
fn load_entries(path: &Path) -> Result<Vec<Entry>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(Value::Array(a)) => Ok(a.into_iter().map(Entry::of_file).collect()),
            Ok(_) => Err("keybindings.json must contain an array of rules.".into()),
            Err(e) => Err(format!("keybindings.json is not valid JSON: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_rules().into_iter().map(Entry::of_rule).collect()),
        Err(e) => Err(e.to_string()),
    }
}

/// The rules in the file this build can read, seeded with the defaults when it is missing.
#[cfg(test)]
fn load_rules(path: &Path) -> Result<Vec<KeybindingRule>, String> {
    Ok(load_entries(path)?.into_iter().filter_map(|entry| entry.rule).collect())
}

fn write_entries(path: &Path, entries: &[Entry]) -> Result<(), String> {
    let written: Vec<&Value> = entries.iter().map(|entry| &entry.written).collect();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&written).expect("JSON values serialize")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn compiled_defaults() -> Vec<ResolvedKeybindingRule> {
    default_rules().iter().filter_map(compile).collect()
}

/// The keybindings in force and the file's issues, as `ServerConfig` carries them.
pub fn resolved(path: &Path) -> Resolved {
    match load_entries(path) {
        Ok(entries) => {
            let custom: Vec<ResolvedKeybindingRule> = entries.iter().filter_map(|entry| entry.rule.as_ref().and_then(compile)).collect();
            let mut issues = vec![];
            let invalid = entries.len() - custom.len();
            if invalid > 0 {
                issues.push(ServerConfigIssue::KeybindingsInvalidEntry(KeybindingsInvalidEntryIssue {
                    message: format!("{invalid} keybinding rule(s) could not be parsed and were ignored."),
                    index: 0.into(),
                }));
            }
            Resolved { keybindings: merge_with_defaults(custom), issues }
        }
        Err(message) => Resolved {
            keybindings: compiled_defaults(),
            issues: vec![ServerConfigIssue::KeybindingsMalformedConfig(KeybindingsMalformedConfigIssue { message })],
        },
    }
}

/// `mergeWithDefaultKeybindings`: defaults for commands the file does not bind.
fn merge_with_defaults(custom: Vec<ResolvedKeybindingRule>) -> Vec<ResolvedKeybindingRule> {
    let defaults = compiled_defaults();
    if custom.is_empty() {
        return defaults;
    }
    let overridden: std::collections::HashSet<&str> = custom.iter().map(|b| b.command.as_str()).collect();
    let mut merged: Vec<ResolvedKeybindingRule> = defaults.into_iter().filter(|b| !overridden.contains(b.command.as_str())).collect();
    merged.extend(custom);
    let skip = merged.len().saturating_sub(MAX_KEYBINDINGS);
    merged.split_off(skip)
}

fn updates() -> &'static tokio::sync::broadcast::Sender<Resolved> {
    static TX: std::sync::OnceLock<tokio::sync::broadcast::Sender<Resolved>> = std::sync::OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(16).0)
}

/// The keybindings and issues after each change.
pub fn subscribe() -> tokio::sync::broadcast::Receiver<Resolved> {
    updates().subscribe()
}

fn parse_error(path: &Path, detail: String) -> Failure {
    Failure::Keybindings { config_path: path.to_string_lossy().into_owned(), detail }
}

/// The rule a request names, if it names one: an object whose `key` and `command` are strings.
fn rule_or_none<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<KeybindingRule>, D::Error> {
    Ok(KeybindingRule::deserialize(Value::deserialize(deserializer)?).ok())
}

/// What `server.upsertKeybinding` is asked (`ServerUpsertKeybindingInput`):
/// a rule, and optionally the rule it takes the place of.
#[derive(Clone, Debug, Deserialize)]
pub struct UpsertInput {
    #[serde(flatten)]
    pub rule: KeybindingRule,
    #[serde(default, deserialize_with = "rule_or_none")]
    pub replace: Option<KeybindingRule>,
}

/// Writes `next` and announces what is now in force.
fn store(path: &Path, next: &[Entry]) -> Result<Resolved, Failure> {
    write_entries(path, next).map_err(|e| parse_error(path, e))?;
    let r = resolved(path);
    let _ = updates().send(r.clone());
    Ok(r)
}

/// `upsertKeybindingRule`: replaces `input.replace` (and any identical rule) with the new rule.
pub fn upsert(path: &Path, input: &UpsertInput) -> Result<Resolved, Failure> {
    let entries = load_entries(path).map_err(|e| parse_error(path, e))?;
    let id = identity(&input.rule);
    let replace = input.replace.as_ref().map(identity);
    let same = |entry: &Entry| entry.rule.as_ref().map(identity).is_some_and(|other| other == id || Some(other) == replace);
    let mut next: Vec<Entry> = entries.into_iter().filter(|entry| !same(entry)).collect();
    next.push(Entry::of_rule(input.rule.clone()));
    let skip = next.len().saturating_sub(MAX_KEYBINDINGS);
    store(path, &next.split_off(skip))
}

/// `removeKeybindingRule`: removes every rule that is the same as `rule`.
pub fn remove(path: &Path, rule: &KeybindingRule) -> Result<Resolved, Failure> {
    let entries = load_entries(path).map_err(|e| parse_error(path, e))?;
    let id = identity(rule);
    let next: Vec<Entry> = entries.into_iter().filter(|entry| entry.rule.as_ref().map(identity) != Some(id.clone())).collect();
    store(path, &next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(value: Value) -> KeybindingRule {
        serde_json::from_value(value).unwrap()
    }

    fn upsert_input(value: Value) -> UpsertInput {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn compiled_defaults_match_the_bundled_resolved_defaults() {
        assert_eq!(compiled_defaults(), crate::defaults::keybindings());
        let bundled: Value = serde_json::from_str(crate::defaults::KEYBINDING_RULES).unwrap();
        assert_eq!(json!(default_rules()), bundled, "a default rule is written to the file as it is bundled");
    }

    #[test]
    fn shortcut_and_when_parsing() {
        assert_eq!(json!(parse_shortcut("Cmd+Shift+K").unwrap()), json!({ "key": "k", "metaKey": true, "ctrlKey": false, "shiftKey": true, "altKey": false, "modKey": false }));
        assert_eq!(parse_shortcut("mod++").unwrap().key, "+");
        assert_eq!(parse_shortcut("ctrl+space").unwrap().key, " ");
        assert!(parse_shortcut("a+b").is_none() && parse_shortcut("shift").is_none());
        assert_eq!(parse_shortcut("").unwrap().key, "+", "as in t3code: an empty token list reads as the + key");
        let ast = parse_when("a || !b && (c)").unwrap();
        assert_eq!(encode_when(&ast), "(a || (!(b) && c))");
        assert_eq!(
            json!(ast),
            json!({ "type": "or", "left": { "type": "identifier", "name": "a" },
                "right": { "type": "and", "left": { "type": "not", "node": { "type": "identifier", "name": "b" } }, "right": { "type": "identifier", "name": "c" } } })
        );
        assert!(parse_when("a &&").is_none() && parse_when("(a").is_none() && parse_when("a $ b").is_none());
    }

    #[test]
    fn upsert_and_remove_compare_parsed_forms() {
        let path = std::env::temp_dir().join(format!("domovoi-kb-{}.json", crate::util::new_uuid()));
        let r = upsert(&path, &upsert_input(json!({ "key": "cmd+k", "command": "chat.new", "when": "a&&b" }))).unwrap();
        assert!(r.keybindings.iter().any(|b| b.command.as_str() == "chat.new" && b.shortcut.key == "k" && b.shortcut.meta_key));
        let before = load_rules(&path).unwrap().len();
        upsert(&path, &upsert_input(json!({ "key": "meta+k", "command": "chat.new", "when": "a && b" }))).unwrap();
        assert_eq!(load_rules(&path).unwrap().len(), before, "same rule in another spelling replaces it");
        remove(&path, &rule(json!({ "key": "meta+K", "command": "chat.new", "when": "(a && b)" }))).unwrap();
        assert_eq!(load_rules(&path).unwrap().len(), before - 1);
        std::fs::write(&path, "{").unwrap();
        assert!(matches!(resolved(&path).issues[0], ServerConfigIssue::KeybindingsMalformedConfig(_)));
        let failed = upsert(&path, &upsert_input(json!({ "key": "k", "command": "chat.new" }))).unwrap_err();
        assert!(matches!(failed, Failure::Keybindings { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entries_that_are_no_rules_stay_in_the_file_and_are_reported() {
        let path = std::env::temp_dir().join(format!("domovoi-kb-{}.json", crate::util::new_uuid()));
        let file = json!([{ "key": "mod+j", "command": "chat.new", "comment": "mine" }, "nonsense", { "key": 5, "command": "chat.new" }, { "key": "a+b", "command": "chat.new" }]);
        std::fs::write(&path, file.to_string()).unwrap();
        let r = upsert(&path, &upsert_input(json!({ "key": "mod+y", "command": "diff.toggle", "when": 5, "replace": "nothing" }))).unwrap();
        assert_eq!(
            json!(r.issues),
            json!([{ "kind": "keybindings.invalid-entry", "message": "3 keybinding rule(s) could not be parsed and were ignored.", "index": 0 }])
        );
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut expected = file.as_array().unwrap().clone();
        expected.push(json!({ "key": "mod+y", "command": "diff.toggle" }));
        assert_eq!(written, json!(expected), "every entry as it was, extra keys included, then the new rule");
        // A rule that cannot be compiled is still removed by its spelling.
        remove(&path, &rule(json!({ "key": "a+b", "command": "chat.new" }))).unwrap();
        assert_eq!(load_rules(&path).unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
