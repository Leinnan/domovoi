//! Project scripts that run on their own (t3code ProjectSetupScriptRunner):
//! the setup script (`runOnWorktreeCreate`) in a new worktree, and the
//! settle script (`runOnSettle`) when a thread settles. Each runs in a
//! terminal of the thread, so its output is there to read; terminals of a
//! script marked `stopOnSettle` close when the thread settles.

#![expect(clippy::disallowed_types, reason = "the user's `t3.json`: its entries, and what else each says, passed on as written")]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use domovoi_model::typed::{ProjectScript, ProjectScriptIcon, ServerSettings};

/// A script resolved for a project: a `ProjectScript` of the settings or of
/// the project, or an entry of a `t3.json`, which may say less (a command is
/// enough there). Written as JSON it is the script as it was given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ProjectScriptIcon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_worktree_create: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on_settle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
    /// What else a `t3.json` or a project says of the script: it is the
    /// user's file, and what this build does not read of it is passed on.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl Script {
    /// The id, or `script` for a script without one.
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or("script")
    }
}

impl From<&ProjectScript> for Script {
    fn from(script: &ProjectScript) -> Self {
        Script {
            id: Some(script.id.clone()),
            name: Some(script.name.clone()),
            command: Some(script.command.clone()),
            icon: Some(script.icon.clone()),
            run_on_worktree_create: Some(script.run_on_worktree_create),
            run_on_settle: script.run_on_settle,
            stop_on_settle: script.stop_on_settle,
            r#async: script.r#async,
            preview_url: script.preview_url.clone(),
            auto_open_preview: script.auto_open_preview,
            rest: Map::new(),
        }
    }
}

fn scripts_of(scripts: &[ProjectScript]) -> Vec<Script> {
    scripts.iter().map(Script::from).collect()
}

/// A project's scripts as t3code resolves them (`resolveProjectScripts`):
/// the per-project override, else the project's own, else the defaults;
/// with none of those, the `scripts` of a `t3.json` in the project root.
///
/// `project_scripts` are the project's own as its record holds them; one
/// that does not read as a script is left out.
pub fn resolve(settings: &ServerSettings, project_id: &str, project_scripts: &[ProjectScript], workspace_root: &str) -> Vec<Script> {
    let overridden = settings.project_settings_overrides.as_ref().and_then(|projects| projects.get(project_id)).and_then(|project| project.default_project_scripts.as_ref());
    if let Some(o) = overridden {
        return scripts_of(o);
    }
    let defaults = scripts_of(settings.default_project_scripts.as_deref().unwrap_or_default());
    match settings.project_script_overrides.as_ref().and_then(|projects| projects.get(project_id)) {
        Some(None) => return defaults,
        Some(Some(a)) => return scripts_of(a),
        None => {}
    }
    if !project_scripts.is_empty() {
        return scripts_of(project_scripts);
    }
    if !defaults.is_empty() {
        return defaults;
    }
    t3_json_scripts(workspace_root)
}

/// `scripts` of `<root>/t3.json`, each given an id from its name if it has none.
pub fn t3_json_scripts(workspace_root: &str) -> Vec<Script> {
    let Ok(text) = std::fs::read_to_string(std::path::Path::new(workspace_root).join("t3.json")) else { return vec![] };
    // The project's own file, of which only `scripts` is read: a list whose entries may or may not be scripts.
    let Ok(file) = serde_json::from_str::<Value>(&text) else {
        tracing::warn!(%workspace_root, "t3.json is not valid JSON");
        return vec![];
    };
    file.get("scripts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|script| Script::deserialize(script).ok())
        .filter(|script| script.command.is_some())
        .map(|mut script| {
            if script.id.is_none() {
                let name = script.name.as_deref().unwrap_or("script");
                script.id = Some(name.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-"));
            }
            script
        })
        .collect()
}

pub fn setup_script(scripts: &[Script]) -> Option<&Script> {
    scripts.iter().find(|s| s.run_on_worktree_create == Some(true))
}

pub fn settle_script(scripts: &[Script]) -> Option<&Script> {
    scripts.iter().find(|s| s.run_on_settle == Some(true))
}

/// (thread, terminal) of scripts that stop when their thread settles.
fn stop_on_settle() -> &'static Mutex<HashSet<(String, String)>> {
    static S: std::sync::OnceLock<Mutex<HashSet<(String, String)>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Runs `script` in the thread terminal `terminal_id` (a login shell in
/// `cwd`, with t3code's `T3CODE_PROJECT_ROOT` / `T3CODE_WORKTREE_PATH`).
/// Returns once the command is typed; the script keeps running there.
pub fn run(thread_id: &str, terminal_id: &str, script: &Script, project_root: &str, worktree_path: Option<&str>, cwd: &str) -> Result<(), String> {
    let state = crate::state::STATE.get().ok_or("the server is not running")?;
    let command = script.command.as_deref().filter(|c| !c.trim().is_empty()).ok_or("the script has no command")?;
    let mut env = HashMap::from([
        ("T3CODE_PROJECT_ROOT".to_owned(), project_root.to_owned()),
        ("NO_COLOR".to_owned(), "1".to_owned()),
        ("FORCE_COLOR".to_owned(), "0".to_owned()),
        ("COLORTERM".to_owned(), String::new()),
    ]);
    if let Some(w) = worktree_path {
        env.insert("T3CODE_WORKTREE_PATH".into(), w.to_owned());
    }
    state.terminals.open(thread_id, terminal_id, cwd, &env).map_err(|e| e.0)?;
    // The login shell reads what was typed ahead once its prompt is up.
    state.terminals.write(thread_id, terminal_id, &format!("{command}\r")).map_err(|e| e.0)?;
    if script.stop_on_settle == Some(true) {
        stop_on_settle().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((thread_id.to_owned(), terminal_id.to_owned()));
    }
    Ok(())
}

/// A thread settled: close its stop-on-settle terminals, then run the
/// project's settle script (in the thread's worktree, else the project root).
pub async fn thread_settled(thread_id: &str) {
    let Some(state) = crate::state::STATE.get() else { return };
    let stopping: Vec<(String, String)> = {
        let mut set = stop_on_settle().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mine: Vec<_> = set.iter().filter(|(t, _)| t == thread_id).cloned().collect();
        for k in &mine {
            set.remove(k);
        }
        mine
    };
    for (t, term) in stopping {
        let _ = state.terminals.close(&t, &term);
    }
    let Ok(Some(thread)) = state.store.app_thread(thread_id) else { return };
    let Ok(Some(project)) = state.store.get_project(thread.project_id.as_str()) else { return };
    let settings = state.settings.read().await.clone();
    let scripts = resolve(&settings, &project.id, &project.scripts, &project.workspace_root);
    let Some(script) = settle_script(&scripts) else { return };
    let worktree = thread.worktree_path.as_deref().filter(|w| std::path::Path::new(w).is_dir());
    let terminal = format!("settle-{}-{}", script.id(), &crate::util::new_uuid()[..8]);
    if let Err(e) = run(thread_id, &terminal, script, &project.workspace_root, worktree, worktree.unwrap_or(&project.workspace_root)) {
        tracing::warn!(%thread_id, "settle script did not start: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Value) -> ServerSettings {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn scripts_resolve_from_overrides_project_defaults_then_t3_json() {
        let root = std::env::temp_dir().join(format!("domovoi-scripts-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("t3.json"), r#"{ "scripts": [{ "name": "Setup Worktree", "command": "make deps", "runOnWorktreeCreate": true, "note": "mine" }, { "name": "bad" }] }"#).unwrap();
        let root_s = root.to_string_lossy().into_owned();
        let script = |id: &str, command: &str| json!({ "id": id, "name": id, "command": command, "icon": "play", "runOnWorktreeCreate": false });
        let own: Vec<ProjectScript> = serde_json::from_value(json!([script("own", "a")])).unwrap();
        let none = settings(json!({}));
        assert_eq!(json!(resolve(&none, "p", &own, &root_s)), json!(own), "a script is passed on as it was given");
        let from_file = resolve(&none, "p", &[], &root_s);
        assert_eq!(from_file.len(), 1, "entries without a command are dropped");
        assert_eq!(json!(from_file[0]), json!({ "id": "setup-worktree", "name": "Setup Worktree", "command": "make deps", "runOnWorktreeCreate": true, "note": "mine" }));
        assert_eq!(setup_script(&from_file).unwrap().command.as_deref(), Some("make deps"));
        let mut settle = script("d", "b");
        settle["runOnSettle"] = json!(true);
        let defaults = settings(json!({ "defaultProjectScripts": [settle] }));
        assert_eq!(resolve(&defaults, "p", &[], &root_s)[0].id(), "d");
        assert_eq!(settle_script(&resolve(&defaults, "p", &[], &root_s)).unwrap().id(), "d");
        let cleared = settings(json!({ "defaultProjectScripts": [], "projectScriptOverrides": { "p": null } }));
        assert!(resolve(&cleared, "p", &own, &root_s).is_empty(), "a null override means the defaults");
        let folded = settings(json!({ "projectSettingsOverrides": { "p": { "defaultProjectScripts": [script("o", "c")] } } }));
        assert_eq!(resolve(&folded, "p", &own, &root_s)[0].id(), "o");
        let _ = std::fs::remove_dir_all(&root);
    }
}
