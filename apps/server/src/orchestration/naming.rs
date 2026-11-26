//! Names a thread and its branch with the text generation model, after the
//! command that asked committed (t3code ThreadTitleService and the branch
//! rename of ThreadLaunchService). Results come back as commands, so they
//! are ordered with everything else; a failure leaves the seed title or the
//! temporary branch in place.

use super::commands::Orchestrator;
use std::sync::Arc;
use domovoi_model::typed::{ConversationMessage, MessageRole, TitleRegenerationStatus};
use domovoi_model::typed::command::{ThreadMetadataUpdate, ThreadTitleRegenerationComplete, TitleRegenerationOutcome};

/// The server's settings.
async fn settings() -> crate::settings::Settings {
    match crate::state::STATE.get() {
        Some(s) => s.settings.read().await.clone(),
        None => Default::default(),
    }
}

/// The model that writes titles and branch names, as `text_gen` takes it.
fn text_model(settings: &crate::settings::Settings) -> domovoi_model::typed::TextGenerationModelSelection {
    crate::text_gen::model_for(settings, crate::text_gen::Writer::Text)
}

/// The thread's messages as the title prompt reads them: the first user
/// message, or (regenerating) every message labelled by role.
fn thread_text(messages: &[Arc<ConversationMessage>], whole: bool) -> String {
    let mut messages: Vec<&ConversationMessage> = messages.iter().map(|message| &**message).collect();
    messages.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
    if !whole {
        return messages.iter().find(|m| m.role == MessageRole::User).map(|m| m.text.clone()).unwrap_or_default();
    }
    messages.iter().filter(|m| !m.text.trim().is_empty())
        .map(|m| format!("{}: {}", if m.role == MessageRole::User { "USER" } else { "ASSISTANT" }, m.text.trim()))
        .collect::<Vec<_>>().join("\n\n")
}

pub async fn title(orch: &Orchestrator, thread_id: &str, request_id: &str, regenerate: bool) {
    let store = orch.store();
    let (Ok(Some(thread)), Ok(messages)) = (store.app_thread(thread_id), store.records::<ConversationMessage>(thread_id)) else { return };
    let settings = settings().await;
    let project = store.get_project(thread.project_id.as_str()).ok().flatten().map(|p| p.title);
    let previous = regenerate.then(|| thread.title.clone());
    let prompt = crate::text_gen::thread_title_prompt(&thread_text(&messages, regenerate), project.as_deref(), previous.as_deref(), settings.thread_title_instructions.as_deref());
    let req = crate::text_gen::Request { operation: "generateThreadTitle", cwd: None, prompt, model: text_model(&settings), investigate: false };
    let outcome = |status: TitleRegenerationStatus, message: Option<String>| Some(TitleRegenerationOutcome { status, message });
    let (title, outcome) = match crate::text_gen::generate(req).await {
        Ok(answer) => match answer.title.as_deref().and_then(crate::text_gen::clean_title) {
            Some(t) if previous.as_deref() == Some(t.as_str()) => (None, outcome(TitleRegenerationStatus::Unchanged, None)),
            Some(t) => (Some(t), outcome(TitleRegenerationStatus::Renamed, None)),
            None => (None, outcome(TitleRegenerationStatus::Failed, Some("The model returned no title.".into()))),
        },
        Err(e) => {
            tracing::info!(%thread_id, "thread title not generated: {}", e.detail);
            (None, outcome(TitleRegenerationStatus::Failed, Some(e.detail)))
        }
    };
    let complete = ThreadTitleRegenerationComplete {
        command_id: format!("title:{request_id}").into(),
        thread_id: thread_id.into(),
        request_id: request_id.into(),
        title,
        outcome,
    };
    if let Err(e) = orch.dispatch(complete.into()).await {
        tracing::warn!(%thread_id, "could not record the thread title: {e}");
    }
}

/// The branch name t3code's `branchNamingMode` makes of the model's answer.
pub fn branch_for(fragment: &str, mode: &str, prefix: &str) -> String {
    let prefix = prefix.trim().trim_matches('/');
    if mode != "static" || prefix.is_empty() { fragment.to_owned() } else { format!("{prefix}/{fragment}") }
}

/// The branch a thread started on an issue works on: the issue's key and
/// title (`12-login-crashes`, `eng-4-login-crashes`, which is also how
/// Linear finds an issue's branch), under the prefix as `branch_for` puts it.
pub fn issue_branch(display_key: &str, title: &str, mode: &str, prefix: &str) -> String {
    const LONGEST: usize = 50;
    let fragment = crate::rpc::source_control::git_workflow::sanitize_branch_fragment(&format!("{}-{title}", display_key.trim_start_matches('#')));
    let fragment = match fragment.char_indices().nth(LONGEST) {
        // Cut after a whole word where there is one.
        Some((end, _)) => {
            let cut = &fragment[..end];
            cut.rfind('-').filter(|at| *at > LONGEST / 2).map_or(cut, |at| &cut[..at]).trim_end_matches(['-', '/', '_', '.'])
        }
        None => fragment.as_str(),
    };
    branch_for(fragment, mode, prefix)
}

pub async fn branch(orch: &Orchestrator, thread_id: &str) {
    let store = orch.store();
    let (Ok(Some(thread)), Ok(messages)) = (store.app_thread(thread_id), store.records::<ConversationMessage>(thread_id)) else { return };
    let (Some(worktree), Some(current)) = (thread.worktree_path.as_deref(), thread.branch.as_deref()) else { return };
    let settings = settings().await;
    let mode = settings.branch_naming_mode.as_ref().map_or("static", |mode| mode.as_str());
    let prompt = crate::text_gen::branch_name_prompt(&thread_text(&messages, false), mode, settings.branch_name_instructions.as_deref());
    let req = crate::text_gen::Request { operation: "generateBranchName", cwd: None, prompt, model: text_model(&settings), investigate: false };
    let fragment = match crate::text_gen::generate(req).await {
        Ok(a) => a.branch.as_deref().and_then(crate::text_gen::clean_branch),
        Err(e) => {
            tracing::info!(%thread_id, "branch name not generated: {}", e.detail);
            None
        }
    };
    let Some(fragment) = fragment else { return };
    let wanted = branch_for(&fragment, mode, settings.branch_name_prefix.as_deref().unwrap_or("domovoi"));
    if let Some(name) = rename_branch(worktree, current, &wanted).await {
        let renamed = ThreadMetadataUpdate {
            command_id: format!("branch-name:{thread_id}").into(),
            thread_id: thread_id.into(),
            title: None,
            regenerate_title: None,
            branch: Some(Some(name)),
            worktree_path: None,
            expected_worktree_path: None,
            expected_branch: None,
            expected_empty: None,
            limit_recovery: None,
            linked_pull_request: None,
        };
        if let Err(e) = orch.dispatch(renamed.into()).await {
            tracing::warn!(%thread_id, "could not record the branch name: {e}");
        }
    }
}

async fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process::git(cwd);
    cmd.args(args);
    let o = crate::process::output(cmd, crate::process::QUICK).await.ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// Renames the worktree's branch from `current` to `wanted` (or `wanted-2`,
/// ... when taken), only while it is still `current` and was never pushed.
pub async fn rename_branch(worktree: &str, current: &str, wanted: &str) -> Option<String> {
    if git(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.as_deref() != Some(current) {
        return None;
    }
    if git(worktree, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).await.is_some() {
        return None;
    }
    let base = crate::vcs_ops::usable_branch_name(worktree, wanted).await;
    let mut name = base.clone();
    for n in 2..50 {
        if git(worktree, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")]).await.is_none() {
            break;
        }
        name = format!("{base}-{n}");
    }
    if name == current {
        return None;
    }
    git(worktree, &["branch", "-m", current, &name]).await.map(|_| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_follow_the_naming_mode() {
        assert_eq!(branch_for("add-search", "static", "t3"), "t3/add-search");
        assert_eq!(branch_for("add-search", "static", ""), "add-search");
        assert_eq!(branch_for("feat/add-search", "semantic", "t3"), "feat/add-search");
        assert_eq!(branch_for("jira-12-x", "custom", "t3"), "jira-12-x");
    }

    #[test]
    fn an_issues_branch_is_its_key_and_title() {
        assert_eq!(issue_branch("#12", "Login crashes on submit", "static", "domovoi"), "domovoi/12-login-crashes-on-submit");
        assert_eq!(issue_branch("ENG-4", "Żółć: can't “save”", "static", "domovoi"), "domovoi/eng-4-cant-save");
        assert_eq!(issue_branch("#7", "", "semantic", "domovoi"), "7");
        let long = issue_branch("#1234", "The quick brown fox jumps over the lazy dog and keeps on running for a very long time", "static", "");
        assert_eq!(long, "1234-the-quick-brown-fox-jumps-over-the-lazy-dog");
        assert!(issue_branch("#1", &"x".repeat(200), "static", "").len() <= 50);
    }

    #[tokio::test]
    async fn a_temporary_branch_is_renamed_once_and_never_after_a_push() {
        let d = std::env::temp_dir().join(format!("domovoi-rename-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        let run = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&d).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap().status.success(), "{args:?}");
        run(&["init", "-q", "-b", "t3/tmp1"]);
        run(&["commit", "-q", "--allow-empty", "-m", "a"]);
        run(&["branch", "t3/add-search"]);
        let cwd = d.to_str().unwrap();
        assert_eq!(rename_branch(cwd, "t3/tmp1", "t3/add-search").await.as_deref(), Some("t3/add-search-2"), "a taken name gets a number");
        assert_eq!(rename_branch(cwd, "t3/tmp1", "t3/other").await, None, "no longer on the temporary branch");
        let _ = std::fs::remove_dir_all(&d);
    }
}
