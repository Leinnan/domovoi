//! `orchestration.launchThread`: makes the thread's workspace, creates the
//! thread, and sends its first message.

use super::{Effect, LaunchFailure, LaunchRefusal, Orchestrator, now};
use crate::orchestration::events::enc;
use domovoi_model::typed::command::MessageDispatch;
use domovoi_model::typed::{
    Actor, AppThread, CreationSource, EventBody, ThreadChange, ThreadId, ThreadLaunchInput, ThreadLaunchResult, ThreadLineage,
    WorkspaceStrategy,
};

/// What a thread that was launched is answered with.
pub type LaunchResult = ThreadLaunchResult;

impl Orchestrator {
    /// `orchestration.launchThread`: create the thread, then dispatch its initial message.
    pub async fn launch_thread(&self, input: ThreadLaunchInput) -> Result<LaunchResult, LaunchFailure> {
        let (command_id, project_id) = (input.command_id.clone(), input.project_id.clone());
        self.launch(input).await.map_err(|refusal| LaunchFailure { command_id: Some(command_id), project_id: Some(project_id), refusal })
    }

    async fn launch(&self, input: ThreadLaunchInput) -> Result<LaunchResult, LaunchRefusal> {
        let command_id = input.command_id.as_str();
        // Keyed by the thread, or by the command when the id is generated
        // here: a launch sent twice still makes one thread. Provisioning a
        // worktree below holds up only this launch.
        let key = input.thread_id.as_ref().map_or_else(|| format!("launch:{command_id}"), |id| id.as_str().to_owned());
        let _g = self.threads.lock(&key).await;
        if let Some(answered) = self.store.command_receipt::<LaunchResult>(command_id)? {
            return Ok(answered);
        }
        let thread_id = input.thread_id.clone().unwrap_or_else(|| ThreadId::from(format!("thread:project:{}:{}", input.project_id, crate::util::new_uuid())));
        if self.store.app_thread(thread_id.as_str())?.is_some() {
            return Err(LaunchRefusal::ThreadExists(thread_id));
        }
        let project = self.store.get_project(input.project_id.as_str())?.ok_or(LaunchRefusal::UnknownProject)?;
        let at = now();
        let title = if input.title.trim().is_empty() { "New thread" } else { input.title.as_str() };
        let (worktree_path, branch): (Option<String>, Option<String>) = match &input.workspace_strategy {
            WorkspaceStrategy::ExistingWorktree { worktree_path, branch } => {
                if !std::path::Path::new(worktree_path).is_dir() {
                    return Err(LaunchRefusal::NoSuchWorktree(worktree_path.clone()));
                }
                let branch = match branch {
                    Some(branch) => Some(branch.clone()),
                    None => checked_out_branch(worktree_path).await,
                };
                (Some(worktree_path.clone()), branch)
            }
            WorkspaceStrategy::Worktree { base_ref, branch, .. } => {
                let settings = match crate::state::STATE.get() {
                    Some(s) => s.settings.read().await.clone(),
                    None => Default::default(),
                };
                let scripts = crate::project_scripts::resolve(&settings, &project.id, &project.scripts, &project.workspace_root);
                let setup = crate::project_scripts::setup_script(&scripts).cloned();
                let (path, branch) = provision_worktree(&project.workspace_root, thread_id.as_str(), base_ref, branch.as_deref(), setup.as_ref())
                    .await
                    .map_err(LaunchRefusal::Worktree)?;
                (Some(path), Some(branch))
            }
            WorkspaceStrategy::Root { branch } => {
                // A thread in the scratch ("No project") project works in a folder of its own.
                let text = input.initial_message.as_ref().map_or(title, |message| message.text.as_str());
                match crate::managed::folder_for_thread(&project.workspace_root, thread_id.as_str(), text)? {
                    Some(folder) => (Some(folder), None),
                    None => match branch {
                        Some(branch) => (None, Some(branch.clone())),
                        None => (None, checked_out_branch(&project.workspace_root).await),
                    },
                }
            }
        };
        let thread = AppThread {
            created_by: input.created_by.clone().unwrap_or(Actor::User),
            creation_source: input.creation_source.clone().unwrap_or(CreationSource::Web),
            id: thread_id.clone(),
            project_id: project.id.as_str().into(),
            title: title.to_owned(),
            provider_instance_id: input.model_selection.instance_id.clone(),
            model_selection: input.model_selection.clone(),
            runtime_mode: input.runtime_mode.clone(),
            interaction_mode: input.interaction_mode.clone(),
            branch,
            worktree_path,
            linked_pull_request: None,
            pull_requests: Some(vec![]),
            branch_pull_request: None,
            active_provider_thread_id: None,
            history_origin: None,
            link_origin: None,
            // A task an agent delegated (`delegate_task`): its completion goes back there.
            delegated_from: input.delegated_from.clone().map(Some),
            lineage: ThreadLineage { parent_thread_id: None, relationship_to_parent: None, root_thread_id: thread_id.clone(), side_chat: None },
            forked_from: None,
            created_at: at.clone(),
            updated_at: at,
            archived_at: None,
            settled_override: Some(None),
            settled_at: Some(None),
            unsettled_at: None,
            snoozed_until: Some(None),
            snoozed_at: Some(None),
            last_snooze_wake_at: None,
            limit_recovery: None,
            guest_share: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            tags: None,
            last_visited_at: Some(None),
            opens: None,
            title_regeneration: None,
            last_title_regeneration: None,
            rollback_request_id: None,
            rollback_failure: None,
            deleted_at: None,
        };
        // Created, first message, receipt: all or nothing, as in `dispatch`.
        self.store.db().clone().batch(|| self.launch_records(&input, thread))
    }

    /// The writes of a launch, after the workspace is ready (inside a batch).
    fn launch_records(&self, input: &ThreadLaunchInput, thread: AppThread) -> Result<LaunchResult, LaunchRefusal> {
        let (command_id, thread_id) = (&input.command_id, thread.id.clone());
        let created_by = thread.created_by.clone();
        let creation_source = thread.creation_source.clone();
        let origin = Self::origin(command_id, Some(&thread.provider_instance_id));
        self.store.append_event(thread_id.as_str(), EventBody::Thread { change: ThreadChange::Created, thread: Box::new(thread) }, &origin)?;
        let mut sequence = None;
        if let Some(message) = &input.initial_message {
            let message_id = match &message.message_id {
                Some(id) => id.clone(),
                None => format!("message:thread:{}:ordinal:1:{}", enc(thread_id.as_str()), crate::util::new_uuid()).into(),
            };
            let mut first = MessageDispatch::new(format!("{command_id}:initial-message"), thread_id.clone(), message_id, message.text.as_str(), created_by, creation_source);
            first.attachments = message.attachments.clone();
            first.title_seed = (input.generate_title == Some(true)).then(|| input.title.clone());
            first.model_selection = Some(input.model_selection.clone());
            sequence = Some(self.message_dispatch(&first).map_err(LaunchRefusal::InitialMessage)?.sequence);
        }
        // A worktree made on a branch nobody named gets its name from the first message.
        let unnamed_branch = matches!(&input.workspace_strategy, WorkspaceStrategy::Worktree { branch, .. } if branch.as_deref().is_none_or(|b| b.trim().is_empty()));
        if unnamed_branch && input.initial_message.is_some() {
            self.send_effect(Effect::NameBranch { thread_id: thread_id.as_str().to_owned() });
        }
        let projection = crate::orchestration::projection::thread_projection(&self.store, thread_id.as_str())?.ok_or(LaunchRefusal::ThreadVanished)?;
        let result = LaunchResult { thread_id, projection, resumed: false, sequence };
        self.store.put_command_receipt(command_id.as_str(), &result)?;
        Ok(result)
    }
}

/// The branch checked out in `cwd`, if it is a repository on a branch.
async fn checked_out_branch(cwd: &str) -> Option<String> {
    crate::vcs::checked_out_branch(cwd).await
}

/// Creates a thread's worktree on a new branch from `base`, reporting
/// progress to the worktree setup tracker (t3code `ThreadLaunchService`).
/// `requested` is the branch to make; without one a temporary name is used.
/// Returns the worktree's path and its branch.
async fn provision_worktree(cwd: &str, thread_id: &str, base: &str, requested: Option<&str>, setup: Option<&crate::project_scripts::Script>) -> Result<(String, String), String> {
    use crate::vcs_ops;
    use crate::worktree_setup::{Phase, StageId, StageStatus};
    let tracker = crate::worktree_setup::tracker();
    let generated = requested.is_none();
    let mut branch = requested.map_or_else(vcs_ops::temporary_branch_name, str::to_owned);
    tracker.start(thread_id, Some(&branch), Some(base));
    let fail = |m: String| {
        tracker.finish(thread_id, Phase::Failed, Some(&m));
        m
    };
    tracker.stage(thread_id, StageId::Fetch, StageStatus::Skipped);
    if generated {
        branch = vcs_ops::usable_branch_name(cwd, &branch).await;
    }
    if tracker.is_cancelled(thread_id) {
        tracker.finish(thread_id, Phase::Cancelled, None);
        return Err("Worktree setup was cancelled.".into());
    }
    tracker.stage(thread_id, StageId::Checkout, StageStatus::Running);
    let created = vcs_ops::create_worktree(cwd, base, Some(&branch), None)
        .await
        .map_err(|e| fail(e.detail))?;
    let path = created.worktree.path;
    tracker.set_worktree(thread_id, &path, &branch);
    tracker.stage(thread_id, StageId::Checkout, StageStatus::Done);
    if std::path::Path::new(&path).join(".gitmodules").is_file() {
        tracker.stage(thread_id, StageId::Submodules, StageStatus::Running);
        // Under the launch's lock: a submodule that asks for a password, or
        // never answers, fails here instead of holding the thread.
        let mut submodules = crate::process::git(&path);
        submodules.args(["submodule", "update", "--init", "--recursive"]);
        let ok = crate::process::output(submodules, crate::process::SLOW).await.is_ok_and(|o| o.status.success());
        tracker.stage(thread_id, StageId::Submodules, if ok { StageStatus::Done } else { StageStatus::Warning });
    } else {
        tracker.stage(thread_id, StageId::Submodules, StageStatus::Skipped);
    }
    // The project's setup script runs in a thread terminal in the new
    // worktree; the agent starts while it runs (t3code: async by default).
    match setup {
        None => tracker.stage(thread_id, StageId::SetupScript, StageStatus::Skipped),
        Some(script) => {
            tracker.stage(thread_id, StageId::SetupScript, StageStatus::Running);
            let terminal = format!("setup-{}", script.id());
            match crate::project_scripts::run(thread_id, &terminal, script, cwd, Some(&path), &path) {
                Ok(()) => tracker.stage(thread_id, StageId::SetupScript, StageStatus::Done),
                Err(e) => {
                    tracing::warn!(%thread_id, "setup script did not start: {e}");
                    tracker.stage(thread_id, StageId::SetupScript, StageStatus::Warning);
                }
            }
        }
    }
    tracker.stage(thread_id, StageId::Agent, StageStatus::Done);
    tracker.finish(thread_id, Phase::Done, None);
    Ok((path, branch))
}
