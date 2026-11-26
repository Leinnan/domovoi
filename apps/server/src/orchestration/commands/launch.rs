//! `orchestration.launchThread`: makes the thread's workspace, creates the
//! thread, and sends its first message.

use super::{Effect, LaunchFailure, LaunchRefusal, Orchestrator, now};
use crate::orchestration::events::enc;
use domovoi_model::typed::command::{InitialMessage, MessageDispatch};
use domovoi_model::typed::{
    Actor, AppThread, CreationSource, EventBody, ISSUE_DEFAULT_PROMPT, MessageIssue, ThreadChange, ThreadId, ThreadIssueLink, ThreadIssueLinkSource,
    ThreadIssueRole, ThreadLaunchInput, ThreadLaunchResult, ThreadLineage, WorkspaceStrategy,
};

/// What a thread that was launched is answered with.
pub type LaunchResult = ThreadLaunchResult;

impl Orchestrator {
    /// `orchestration.launchThread`: create the thread, then dispatch its initial message.
    pub async fn launch_thread(&self, input: ThreadLaunchInput) -> Result<LaunchResult, LaunchFailure> {
        self.launch_thread_on_issue(input, None).await
    }

    /// A launch with the issue its thread is started on already read
    /// (`rpc::issues::snapshot`): reading a tracker is not the orchestrator's
    /// to do. A launch that names an issue and comes without one is refused,
    /// so no thread starts on an issue its agent was not shown.
    pub async fn launch_thread_on_issue(&self, input: ThreadLaunchInput, issue: Option<MessageIssue>) -> Result<LaunchResult, LaunchFailure> {
        let (command_id, project_id) = (input.command_id.clone(), input.project_id.clone());
        self.launch(input, issue).await.map_err(|refusal| LaunchFailure { command_id: Some(command_id), project_id: Some(project_id), refusal })
    }

    /// What a launch was answered with, if it was carried out already: a
    /// launch sent again is answered from here, before anything is read for it.
    pub fn launch_answered(&self, command_id: &str) -> Result<Option<LaunchResult>, LaunchRefusal> {
        Ok(self.store.command_receipt::<LaunchResult>(command_id)?)
    }

    async fn launch(&self, input: ThreadLaunchInput, issue: Option<MessageIssue>) -> Result<LaunchResult, LaunchRefusal> {
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
        let issue = match (&input.issue, issue) {
            (Some(named), Some(read)) => Some((named.issue.project_id.clone(), read)),
            (Some(_), None) => return Err(LaunchRefusal::Issue("It was not read before the launch.".into())),
            (None, _) => None,
        };
        // A thread started on an issue is called what the issue is.
        let issue_title = issue.as_ref().map(|(_, read)| format!("{} {}", read.card.display_key, read.card.title));
        let title = match &issue_title {
            Some(title) => title.as_str(),
            None if input.title.trim().is_empty() => "New thread",
            None => input.title.as_str(),
        };
        let mut base_branch = None;
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
            WorkspaceStrategy::Worktree { base_ref, branch, start_from_origin } => {
                let settings = match crate::state::STATE.get() {
                    Some(s) => s.settings.read().await.clone(),
                    None => Default::default(),
                };
                let scripts = crate::project_scripts::resolve(&settings, &project.id, &project.scripts, &project.workspace_root);
                let setup = crate::project_scripts::setup_script(&scripts).cloned();
                let named = branch.as_deref().filter(|branch| !branch.trim().is_empty());
                let request = match (named, &issue) {
                    (Some(branch), _) => BranchRequest::Named(branch),
                    // A thread started on an issue works on a branch named for it: nothing is generated.
                    (None, Some((_, read))) => {
                        let mode = settings.branch_naming_mode.as_ref().map_or("static", |mode| mode.as_str());
                        let prefix = settings.branch_name_prefix.as_deref().unwrap_or("domovoi");
                        BranchRequest::Derived(crate::orchestration::naming::issue_branch(&read.card.display_key, &read.card.title, mode, prefix))
                    }
                    (None, None) => BranchRequest::Temporary,
                };
                let worktree = NewWorktree {
                    base: base_ref,
                    request,
                    from_origin: start_from_origin.unwrap_or(settings.new_worktrees_start_from_origin != Some(false)),
                    setup: setup.as_ref(),
                };
                base_branch = crate::vcs_ops::base_branch_name(&project.workspace_root, base_ref).await;
                let (path, branch) = provision_worktree(&project.workspace_root, thread_id.as_str(), worktree).await.map_err(LaunchRefusal::Worktree)?;
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
            base_branch,
            linked_pull_request: None,
            pull_requests: Some(vec![]),
            issues: issue.as_ref().map(|(project_id, read)| {
                vec![ThreadIssueLink {
                    key: read.card.key.clone(),
                    project_id: project_id.clone(),
                    url: read.card.url.clone(),
                    source: ThreadIssueLinkSource::Manual,
                    linked_at: at.clone(),
                    display_key: read.card.display_key.clone(),
                    title: read.card.title.clone(),
                    role: Some(ThreadIssueRole::Focus),
                }]
            }),
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
        let issue = issue.map(|(_, read)| read);
        self.store.db().clone().batch(|| self.launch_records(&input, thread, issue))
    }

    /// The writes of a launch, after the workspace is ready (inside a batch).
    fn launch_records(&self, input: &ThreadLaunchInput, thread: AppThread, issue: Option<MessageIssue>) -> Result<LaunchResult, LaunchRefusal> {
        let (command_id, thread_id) = (&input.command_id, thread.id.clone());
        let created_by = thread.created_by.clone();
        let creation_source = thread.creation_source.clone();
        let origin = Self::origin(command_id, Some(&thread.provider_instance_id));
        self.store.append_event(thread_id.as_str(), EventBody::Thread { change: ThreadChange::Created, thread: Box::new(thread) }, &origin)?;
        let mut sequence = None;
        // A thread started on an issue always has a first message: the issue is what it says.
        let unworded = issue.as_ref().map(|_| InitialMessage { message_id: None, text: String::new(), context: None, attachments: vec![] });
        if let Some(message) = input.initial_message.as_ref().or(unworded.as_ref()) {
            let message_id = match &message.message_id {
                Some(id) => id.clone(),
                None => format!("message:thread:{}:ordinal:1:{}", enc(thread_id.as_str()), crate::util::new_uuid()).into(),
            };
            let text = if issue.is_some() && message.text.trim().is_empty() { ISSUE_DEFAULT_PROMPT } else { message.text.as_str() };
            let mut first = MessageDispatch::new(format!("{command_id}:initial-message"), thread_id.clone(), message_id, text, created_by, creation_source);
            first.attachments = message.attachments.clone();
            // The issue's title is the thread's: nothing is generated over it.
            first.title_seed = (input.generate_title == Some(true) && issue.is_none()).then(|| input.title.clone());
            first.issue = issue;
            first.model_selection = Some(input.model_selection.clone());
            sequence = Some(self.message_dispatch(&first).map_err(LaunchRefusal::InitialMessage)?.sequence);
        }
        // A worktree made on a branch nobody named gets its name from the first message.
        // One started on an issue was named for the issue already.
        let unnamed_branch = matches!(&input.workspace_strategy, WorkspaceStrategy::Worktree { branch, .. } if branch.as_deref().is_none_or(|b| b.trim().is_empty()));
        if unnamed_branch && input.initial_message.is_some() && input.issue.is_none() {
            self.send_effect(Effect::NameBranch { thread_id: thread_id.as_str().to_owned() });
        }
        // As the client that launched it is sent the thread.
        let projection = crate::orchestration::projection::thread_projection(&self.store, thread_id.as_str())?.ok_or(LaunchRefusal::ThreadVanished)?.for_client();
        let result = LaunchResult { thread_id, projection, resumed: false, sequence };
        self.store.put_command_receipt(command_id.as_str(), &result)?;
        Ok(result)
    }
}

/// The branch checked out in `cwd`, if it is a repository on a branch.
async fn checked_out_branch(cwd: &str) -> Option<String> {
    crate::vcs::checked_out_branch(cwd).await
}

/// The branch a new worktree is made on.
enum BranchRequest<'a> {
    /// A temporary name, until the thread's first message names it.
    Temporary,
    /// The name the launch gave: one that is taken fails the launch.
    Named(&'a str),
    /// A name the server worked out (from an issue): one that is taken gets a number.
    Derived(String),
}

/// What a launch asks of its new worktree.
struct NewWorktree<'a> {
    /// What to cut it from: a branch, `HEAD`, or any ref.
    base: &'a str,
    request: BranchRequest<'a>,
    /// Start from the remote's version of `base` (`vcs_ops::origin_base`).
    from_origin: bool,
    setup: Option<&'a crate::project_scripts::Script>,
}

/// Creates a thread's worktree on a new branch, reporting progress to the
/// worktree setup tracker. Returns the worktree's path and its branch.
///
/// With `from_origin` the remote's branch is fetched first and the worktree
/// starts from it, so a base branch nobody pulled lately does not put other
/// people's merged work into this thread's diff. A remote that does not
/// answer is a warning and the worktree starts from what is here.
async fn provision_worktree(cwd: &str, thread_id: &str, new: NewWorktree<'_>) -> Result<(String, String), String> {
    use crate::vcs_ops;
    use crate::worktree_setup::{Phase, StageId, StageStatus};
    let tracker = crate::worktree_setup::tracker();
    let NewWorktree { base, request, from_origin, setup } = new;
    let mut branch = match &request {
        BranchRequest::Temporary => vcs_ops::temporary_branch_name(),
        BranchRequest::Named(name) => (*name).to_owned(),
        BranchRequest::Derived(name) => name.clone(),
    };
    tracker.start(thread_id, Some(&branch), Some(base));
    let fail = |m: String| {
        tracker.finish(thread_id, Phase::Failed, Some(&m));
        m
    };
    let mut start = base.to_owned();
    if from_origin {
        tracker.stage(thread_id, StageId::Fetch, StageStatus::Running);
        let status = match vcs_ops::origin_base(cwd, base).await {
            vcs_ops::OriginBase::Fetched { commit } => {
                start = commit;
                StageStatus::Done
            }
            vcs_ops::OriginBase::NotOnARemote | vcs_ops::OriginBase::LocalAhead => StageStatus::Skipped,
            vcs_ops::OriginBase::Unreachable(why) => {
                tracing::info!(%thread_id, "the worktree starts from the local {base}: its remote did not answer: {why}");
                StageStatus::Warning
            }
        };
        tracker.stage(thread_id, StageId::Fetch, status);
    } else {
        tracker.stage(thread_id, StageId::Fetch, StageStatus::Skipped);
    }
    match &request {
        BranchRequest::Temporary => branch = vcs_ops::usable_branch_name(cwd, &branch).await,
        BranchRequest::Derived(_) => branch = vcs_ops::free_branch_name(cwd, &branch).await,
        BranchRequest::Named(_) => {}
    }
    if tracker.is_cancelled(thread_id) {
        tracker.finish(thread_id, Phase::Cancelled, None);
        return Err("Worktree setup was cancelled.".into());
    }
    tracker.stage(thread_id, StageId::Checkout, StageStatus::Running);
    let created = vcs_ops::create_worktree(cwd, &start, Some(&branch), None)
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
