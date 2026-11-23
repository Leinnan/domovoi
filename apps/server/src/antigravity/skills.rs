//! Antigravity's skills (t3code `Drivers/AntigravitySkills.ts`): folders
//! with a `SKILL.md` in the user's `~/.gemini/config/skills` and
//! `~/.gemini/antigravity-cli/skills`, and a project's `.gemini/skills`,
//! `.agents/skills` and legacy `.agent/skills`. The first skill of a name
//! wins, in the agent's own order of those folders.

use std::path::{Path, PathBuf};
use domovoi_model::typed::ServerProviderSkill;

const MAX_SKILL_BYTES: u64 = 1_000_000;
const MAX_SCAN_BYTES: u64 = 8_000_000;
const MAX_SCAN_ENTRIES: usize = 10_000;

struct Budget {
    bytes: u64,
    entries: usize,
}

/// The order the agent reads a folder's children in: Go's `URL.EscapedPath`.
fn sort_key(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-_.~/:@&=+$,;".contains(&b);
        if keep { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) }
    }
    out
}

/// A `SKILL.md`'s name (its own, untrimmed, or the file's) and description.
fn parse(text: &str, file_name: &str) -> Option<(String, Option<String>)> {
    let fields = crate::skills::front_matter(text);
    if !text.trim_start().starts_with("---") {
        return None;
    }
    let name = fields.get("name").cloned().unwrap_or_else(|| file_name.trim_end_matches(".md").trim_end_matches(".MD").to_owned());
    // Native names are not trimmed: one that would need it is not renamed to fit.
    if name.is_empty() || name != name.trim() {
        return None;
    }
    Some((name, fields.get("description").map(|d| d.trim().to_owned()).filter(|d| !d.is_empty())))
}

fn scan(dir: &Path, scope: &str, children: bool, budget: &mut Budget, found: &mut Vec<ServerProviderSkill>) -> bool {
    if !dir.is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return true };
    let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    if names.len() > budget.entries {
        return false;
    }
    budget.entries -= names.len();
    names.sort();
    if let Some(file) = names.iter().find(|n| n.eq_ignore_ascii_case("skill.md")) {
        let path = dir.join(file);
        let Ok(meta) = std::fs::metadata(&path) else { return true };
        if !meta.is_file() {
            return true;
        }
        if meta.len() > MAX_SKILL_BYTES.min(budget.bytes) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { return true };
        budget.bytes -= text.len() as u64;
        if let Some((name, description)) = parse(&text, file)
            && !found.iter().any(|s| s.name == name)
        {
            found.push(ServerProviderSkill {
                name,
                description,
                path: path.to_string_lossy().into_owned(),
                scope: Some(scope.to_owned()),
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: None,
                user_invocable: None,
            });
        }
        return true;
    }
    if children {
        names.sort_by_key(|n| sort_key(n));
        for name in names {
            if !scan(&dir.join(name), scope, false, budget, found) {
                return false;
            }
        }
    }
    true
}

/// The skills a session in `cwd` has (the user's only, without one), by name.
pub fn discover(cwd: Option<&Path>, user_home: &Path) -> Vec<ServerProviderSkill> {
    let [config, cli] = super::user_skill_dirs(&user_home.join(".gemini"));
    let project = |parts: &[&str]| cwd.map(|c| parts.iter().fold(c.to_path_buf(), |p, part| p.join(part)));
    let roots: Vec<(Option<PathBuf>, &str)> = vec![
        (Some(config), "user"),
        (project(&[".gemini", "skills"]), "project"),
        (Some(cli), "user"),
        (project(&[".agents", "skills"]), "project"),
        (project(&[".agent", "skills"]), "project"),
    ];
    let mut budget = Budget { bytes: MAX_SCAN_BYTES, entries: MAX_SCAN_ENTRIES };
    let mut found = vec![];
    for (dir, scope) in roots {
        if let Some(dir) = dir
            && !scan(&dir, scope, true, &mut budget, &mut found)
        {
            tracing::warn!(dir = %dir.display(), "Antigravity skill discovery exceeded its scan limit");
            break;
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_skill_of_a_name_wins_in_the_agents_order() {
        let dir = std::env::temp_dir().join(format!("domovoi-ag-skills-scan-{}", crate::util::new_uuid()));
        let (home, project) = (dir.join("home"), dir.join("p"));
        let skill = |at: PathBuf, name: &str, description: &str| {
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join("SKILL.md"), format!("---\nname: {name}\ndescription: {description}\n---\nBody")).unwrap();
        };
        skill(home.join(".gemini/config/skills/review"), "review", "user review");
        skill(project.join(".gemini/skills/review"), "review", "project review");
        skill(project.join(".agents/skills/deploy"), "deploy", "ship it");
        skill(project.join(".agent/skills/deploy"), "deploy", "legacy");
        let found = discover(Some(&project), &home);
        let shown: Vec<(&str, Option<&str>, Option<&str>)> = found.iter().map(|s| (s.name.as_str(), s.description.as_deref(), s.scope.as_deref())).collect();
        assert_eq!(shown, [("deploy", Some("ship it"), Some("project")), ("review", Some("user review"), Some("user"))]);
        assert_eq!(discover(None, &home).len(), 1, "without a project, the user's only");
        assert_eq!(sort_key("a b?"), "a%20b%3F");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
