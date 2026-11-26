//! Every call the server answers, defined once: its name, whether it answers
//! once or with a stream, the permission a session needs for it, and who
//! calls it. The server registers its handlers against this list and takes
//! each call's permission from it; a method no client of this repository
//! calls yet says which feature it belongs to.
//!
//! What a call carries and answers is not here yet: the server and the
//! client core each still have their own types for that, checked against
//! each other by the tests that run one against the other.

/// Whether a call is answered once, or with items until it is interrupted or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unary,
    Stream,
}

/// Who calls a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The client core (`apps/domovoi-core/src/requests.rs`).
    Client,
    /// No client yet: the server implements a feature no app screen uses.
    /// The text says which. It stays until the feature gets a screen or is
    /// taken out.
    Kept(&'static str),
}

/// One call the server answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    pub name: &'static str,
    pub kind: Kind,
    /// The permission a session needs. A few methods need another one for
    /// some payloads: those rules are the server's (`permissions.rs`).
    pub permission: &'static str,
    pub caller: Caller,
}

/// The method called `name`.
pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.binary_search_by(|method| method.name.cmp(name)).ok().map(|at| &METHODS[at])
}

/// What a session may be allowed to do.
pub mod permission {
    pub const ACCESS_READ: &str = "access:read";
    pub const ACCESS_WRITE: &str = "access:write";
    pub const DIAGNOSTICS_READ: &str = "diagnostics:read";
    pub const ENVIRONMENT_MAINTAIN: &str = "environment:maintain";
    pub const FILESYSTEM_READ: &str = "filesystem:read";
    pub const FILESYSTEM_WRITE: &str = "filesystem:write";
    pub const ORCHESTRATION_OPERATE: &str = "orchestration:operate";
    pub const ORCHESTRATION_READ: &str = "orchestration:read";
    pub const PREVIEW_OPERATE: &str = "preview:operate";
    pub const PROVIDERS_MANAGE: &str = "providers:manage";
    pub const SETTINGS_WRITE: &str = "settings:write";
    pub const SOURCE_CONTROL_WRITE: &str = "source-control:write";
    pub const TERMINAL_OPERATE: &str = "terminal:operate";
    pub const TERMINAL_READ: &str = "terminal:read";
}

/// The features the server implements that no app screen uses yet.
pub mod kept {
    pub const ISSUES: &str = "issues from GitHub, GitLab and Linear (the agent tools read them through the same service)";
    pub const PULL_REQUEST_PAGE: &str = "the rest of a pull request's page: detail, review threads, labels, reviewers, stacks, review guides";
    pub const REPOSITORIES: &str = "repositories: sign-in to a host, lookup, clone and publish";
    pub const PREVIEW: &str = "the preview browser's tabs and the dev servers found on this machine (the agent tools drive the same browser)";
    pub const RESOURCE_TELEMETRY: &str = "resource telemetry over time, and signalling a process";
    pub const TRACE_DIAGNOSTICS: &str = "the trace file's diagnostics";
    pub const LINKED_SERVERS: &str = "linked servers (the agent tools list them)";
    pub const ACP_REGISTRY: &str = "the ACP registry: search, install, uninstall, and an agent's sign-in by URL";
    pub const WORKFLOWS: &str = "workflows, native child work and a command's live output";
    pub const THREAD_READS: &str = "reading parts of a thread without subscribing to it";
    pub const ARTIFACTS: &str = "a thread's artifacts";
    pub const WORKTREE_SETUP: &str = "a worktree's setup script";
    pub const FILES: &str = "files: ignored files, a path's metadata, and attachments uploaded over HTTP (the agent tools hand out the same upload URL)";
    pub const VERSION_CONTROL: &str = "version control: a new repository, the branch alone, a branch's commits, a diff's files, a pull request by its URL";
    pub const ADMINISTRATION: &str = "administration: the configuration read once (the API cases read it back after a change), a keybinding added, text generator models, issue triage, usage rates, a provider CLI's update, a secret source checked";
    pub const SINGLE_CALLS: &str = "a terminal opened by itself, one webhook delivery, signing a provider instance out";
}

macro_rules! methods {
    ($($name:literal => $kind:ident, $permission:ident, $caller:expr;)+) => {
        /// Every method, in the order of their names.
        pub const METHODS: &[Method] = &[
            $(Method { name: $name, kind: Kind::$kind, permission: permission::$permission, caller: $caller },)+
        ];
    };
}

use Caller::{Client, Kept};

methods! {
    "agentSessions.import" => Unary, ORCHESTRATION_OPERATE, Client;
    "agentSessions.scan" => Unary, ORCHESTRATION_READ, Client;
    "artifacts.list" => Unary, ORCHESTRATION_READ, Kept(kept::ARTIFACTS);
    "artifacts.read" => Unary, ORCHESTRATION_READ, Kept(kept::ARTIFACTS);
    "assets.createUrl" => Unary, ORCHESTRATION_READ, Client;
    "assets.persistChatAttachments" => Unary, ORCHESTRATION_OPERATE, Client;
    "attachments.createUploadUrl" => Unary, ORCHESTRATION_OPERATE, Kept(kept::FILES);
    "attachments.delete" => Unary, ORCHESTRATION_OPERATE, Kept(kept::FILES);
    "domovoi.terminals.rename" => Unary, TERMINAL_OPERATE, Client;
    "domovoi.terminals.reopen" => Unary, TERMINAL_OPERATE, Client;
    "domovoi.terminals.subscribe" => Stream, TERMINAL_READ, Client;
    "filesystem.browse" => Unary, FILESYSTEM_READ, Client;
    "filesystem.getMetadata" => Unary, FILESYSTEM_READ, Kept(kept::FILES);
    "git.preparePullRequestThread" => Unary, SOURCE_CONTROL_WRITE, Client;
    "git.resolvePullRequest" => Unary, ORCHESTRATION_READ, Kept(kept::VERSION_CONTROL);
    "git.runStackedAction" => Stream, SOURCE_CONTROL_WRITE, Client;
    "issueTrackers.bindProject" => Unary, SETTINGS_WRITE, Kept(kept::ISSUES);
    "issueTrackers.connect" => Unary, SETTINGS_WRITE, Kept(kept::ISSUES);
    "issueTrackers.list" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.candidates" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.comment" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::ISSUES);
    "issues.comments" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.create" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::ISSUES);
    "issues.detail" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.linkedThreads" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.list" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.react" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::ISSUES);
    "issues.setState" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::ISSUES);
    "issues.templates" => Unary, ORCHESTRATION_READ, Kept(kept::ISSUES);
    "issues.update" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::ISSUES);
    "orchestration.dispatchCommand" => Unary, ORCHESTRATION_OPERATE, Client;
    "orchestration.forceStopThread" => Unary, ORCHESTRATION_OPERATE, Client;
    "orchestration.forgetThreadPermissions" => Unary, ORCHESTRATION_OPERATE, Client;
    "orchestration.getArchivedShellSnapshot" => Unary, ORCHESTRATION_READ, Client;
    "orchestration.getFullThreadDiff" => Unary, ORCHESTRATION_READ, Client;
    "orchestration.getNativeChildWork" => Unary, ORCHESTRATION_READ, Kept(kept::WORKFLOWS);
    "orchestration.getThreadFamily" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.getThreadLinkMetadata" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.getThreadProjection" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.getThreadTitles" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.getTurnDiff" => Unary, ORCHESTRATION_READ, Client;
    "orchestration.getTurnItem" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.getWorkflowScript" => Unary, ORCHESTRATION_READ, Kept(kept::WORKFLOWS);
    "orchestration.getWorktreeThreadIds" => Unary, ORCHESTRATION_READ, Kept(kept::THREAD_READS);
    "orchestration.launchThread" => Unary, ORCHESTRATION_OPERATE, Client;
    "orchestration.searchThread" => Unary, ORCHESTRATION_READ, Client;
    "orchestration.searchThreads" => Unary, ORCHESTRATION_READ, Client;
    "orchestration.stopWorkflow" => Unary, ORCHESTRATION_OPERATE, Kept(kept::WORKFLOWS);
    "orchestration.subscribeCommandOutput" => Stream, ORCHESTRATION_READ, Kept(kept::WORKFLOWS);
    "orchestration.subscribeShell" => Stream, ORCHESTRATION_READ, Client;
    "orchestration.subscribeThread" => Stream, ORCHESTRATION_READ, Client;
    "peerLinks.link" => Unary, ACCESS_WRITE, Kept(kept::LINKED_SERVERS);
    "peerLinks.list" => Unary, ACCESS_READ, Kept(kept::LINKED_SERVERS);
    "peerLinks.unlink" => Unary, ACCESS_WRITE, Kept(kept::LINKED_SERVERS);
    "preview.adjust" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.clearProfile" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.close" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.list" => Unary, ORCHESTRATION_READ, Kept(kept::PREVIEW);
    "preview.open" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.reportProfiles" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.reportStatus" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.resize" => Unary, PREVIEW_OPERATE, Kept(kept::PREVIEW);
    "preview.stopLocalServer" => Unary, TERMINAL_OPERATE, Kept(kept::PREVIEW);
    "projectClone.cancel" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "projectClone.retry" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "projectClone.start" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "projects.createFromThread" => Unary, ORCHESTRATION_OPERATE, Client;
    "projects.createNew" => Unary, ORCHESTRATION_OPERATE, Client;
    "projects.ensureScratch" => Unary, ORCHESTRATION_OPERATE, Client;
    "projects.fetchLfsObject" => Unary, SOURCE_CONTROL_WRITE, Client;
    "projects.listEntries" => Unary, FILESYSTEM_READ, Client;
    "projects.listIgnoredFiles" => Unary, FILESYSTEM_READ, Kept(kept::FILES);
    "projects.mutate" => Unary, ORCHESTRATION_OPERATE, Client;
    "projects.readFile" => Unary, FILESYSTEM_READ, Client;
    "projects.searchContents" => Unary, FILESYSTEM_READ, Client;
    "projects.searchEntries" => Unary, FILESYSTEM_READ, Client;
    "projects.writeFile" => Unary, FILESYSTEM_WRITE, Client;
    "provider.auth.cancel" => Unary, PROVIDERS_MANAGE, Client;
    "provider.auth.complete" => Unary, PROVIDERS_MANAGE, Client;
    "provider.auth.logout" => Unary, PROVIDERS_MANAGE, Kept(kept::SINGLE_CALLS);
    "provider.auth.start" => Unary, PROVIDERS_MANAGE, Client;
    "provider.auth.subscribe" => Stream, PROVIDERS_MANAGE, Client;
    "provider.install.cancel" => Unary, PROVIDERS_MANAGE, Client;
    "provider.install.remove" => Unary, PROVIDERS_MANAGE, Client;
    "provider.install.start" => Unary, PROVIDERS_MANAGE, Client;
    "provider.install.subscribe" => Stream, ORCHESTRATION_READ, Client;
    "pullRequests.activity" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.changeBreakdown" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.checks" => Unary, ORCHESTRATION_READ, Client;
    "pullRequests.comment" => Unary, SOURCE_CONTROL_WRITE, Client;
    "pullRequests.detail" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.diffFileContents" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.filesViewed" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.filterOptions" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.generateReviewGuide" => Unary, ORCHESTRATION_OPERATE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.invalidate" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.labelCandidates" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.linkedThreads" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.list" => Unary, ORCHESTRATION_READ, Client;
    "pullRequests.listStats" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.preview" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.replyToThread" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.reportState" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.requestReviewers" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.reviewGuide" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.reviewerCandidates" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.routing" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.routingIdentity" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.runAction" => Unary, SOURCE_CONTROL_WRITE, Client;
    "pullRequests.setFilesViewed" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.setLabels" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.setReaction" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.setThreadResolution" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.stack" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.submitReview" => Unary, SOURCE_CONTROL_WRITE, Client;
    "pullRequests.subscribeLinkedThreadRefreshes" => Stream, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.subscribeRefreshes" => Stream, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.summary" => Unary, ORCHESTRATION_READ, Client;
    "pullRequests.threadComments" => Unary, ORCHESTRATION_READ, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.update" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "pullRequests.updateComment" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::PULL_REQUEST_PAGE);
    "review.getBranchCommits" => Unary, FILESYSTEM_READ, Kept(kept::VERSION_CONTROL);
    "review.getDiffFileContents" => Unary, FILESYSTEM_READ, Kept(kept::VERSION_CONTROL);
    "review.getDiffPreview" => Unary, FILESYSTEM_READ, Client;
    "scheduledTasks.delete" => Unary, ORCHESTRATION_OPERATE, Client;
    "scheduledTasks.getWebhookDelivery" => Unary, ORCHESTRATION_OPERATE, Kept(kept::SINGLE_CALLS);
    "scheduledTasks.listRuns" => Unary, ORCHESTRATION_READ, Client;
    "scheduledTasks.listWebhookDeliveries" => Unary, ORCHESTRATION_OPERATE, Client;
    "scheduledTasks.rotateWebhookToken" => Unary, ORCHESTRATION_OPERATE, Client;
    "scheduledTasks.runNow" => Unary, ORCHESTRATION_OPERATE, Client;
    "scheduledTasks.setEnabled" => Unary, ORCHESTRATION_OPERATE, Client;
    "scheduledTasks.subscribe" => Stream, ORCHESTRATION_READ, Client;
    "scheduledTasks.upsert" => Unary, ORCHESTRATION_OPERATE, Client;
    "secrets.answerRequest" => Unary, ORCHESTRATION_OPERATE, Client;
    "server.acceptAcpRegistryUrlAuth" => Unary, PROVIDERS_MANAGE, Kept(kept::ACP_REGISTRY);
    "server.checkOtlpEndpoint" => Unary, SETTINGS_WRITE, Kept(kept::TRACE_DIAGNOSTICS);
    "server.checkSecretSource" => Unary, SETTINGS_WRITE, Kept(kept::ADMINISTRATION);
    "server.discoverSourceControl" => Unary, ORCHESTRATION_READ, Kept(kept::VERSION_CONTROL);
    "server.getConfig" => Unary, ORCHESTRATION_READ, Kept(kept::ADMINISTRATION);
    "server.getDatabaseStorage" => Stream, ORCHESTRATION_READ, Client;
    "server.getHostResources" => Unary, ORCHESTRATION_READ, Client;
    "server.getProcessDiagnostics" => Unary, DIAGNOSTICS_READ, Client;
    "server.getProcessResourceHistory" => Unary, DIAGNOSTICS_READ, Kept(kept::RESOURCE_TELEMETRY);
    "server.getResourceTelemetryHistory" => Unary, DIAGNOSTICS_READ, Kept(kept::RESOURCE_TELEMETRY);
    "server.getStorageCleanupReport" => Stream, ORCHESTRATION_READ, Client;
    "server.getTraceDiagnostics" => Unary, DIAGNOSTICS_READ, Kept(kept::TRACE_DIAGNOSTICS);
    "server.getUsageSummary" => Unary, DIAGNOSTICS_READ, Client;
    "server.listTextGeneratorModels" => Unary, SETTINGS_WRITE, Kept(kept::ADMINISTRATION);
    "server.prepareAcpRegistryAgent" => Unary, PROVIDERS_MANAGE, Kept(kept::ACP_REGISTRY);
    "server.prepareTriage" => Unary, ORCHESTRATION_OPERATE, Kept(kept::ADMINISTRATION);
    "server.reclaimDatabaseSpace" => Unary, ENVIRONMENT_MAINTAIN, Client;
    "server.refreshProviders" => Unary, ORCHESTRATION_READ, Client;
    "server.refreshUsageRates" => Unary, DIAGNOSTICS_READ, Kept(kept::ADMINISTRATION);
    "server.removeKeybinding" => Unary, SETTINGS_WRITE, Client;
    "server.retryResourceTelemetry" => Unary, DIAGNOSTICS_READ, Kept(kept::RESOURCE_TELEMETRY);
    "server.runStorageCleanup" => Unary, SETTINGS_WRITE, Client;
    "server.searchAcpRegistry" => Unary, ORCHESTRATION_READ, Kept(kept::ACP_REGISTRY);
    "server.signalProcess" => Unary, ENVIRONMENT_MAINTAIN, Kept(kept::RESOURCE_TELEMETRY);
    "server.uninstallAcpRegistryManagedBinary" => Unary, PROVIDERS_MANAGE, Kept(kept::ACP_REGISTRY);
    "server.updateProvider" => Unary, PROVIDERS_MANAGE, Kept(kept::ADMINISTRATION);
    "server.updateSettings" => Unary, SETTINGS_WRITE, Client;
    "server.upsertKeybinding" => Unary, SETTINGS_WRITE, Kept(kept::ADMINISTRATION);
    "sourceControl.appAccess" => Unary, ORCHESTRATION_READ, Kept(kept::REPOSITORIES);
    "sourceControl.cloneRepository" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.lookupRepository" => Unary, ORCHESTRATION_READ, Kept(kept::REPOSITORIES);
    "sourceControl.publishRepository" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.signInCancel" => Unary, SETTINGS_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.signInStart" => Unary, SETTINGS_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.signInWait" => Unary, SETTINGS_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.star" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::REPOSITORIES);
    "sourceControl.starred" => Unary, ORCHESTRATION_READ, Kept(kept::REPOSITORIES);
    "subscribeAuthAccess" => Stream, ACCESS_READ, Client;
    "subscribeDiscoveredLocalServers" => Stream, ORCHESTRATION_READ, Kept(kept::PREVIEW);
    "subscribePreviewEvents" => Stream, ORCHESTRATION_READ, Kept(kept::PREVIEW);
    "subscribeProjectClones" => Stream, ORCHESTRATION_READ, Kept(kept::REPOSITORIES);
    "subscribeResourceTelemetry" => Stream, DIAGNOSTICS_READ, Kept(kept::RESOURCE_TELEMETRY);
    "subscribeServerConfig" => Stream, ORCHESTRATION_READ, Client;
    "subscribeServerLifecycle" => Stream, ORCHESTRATION_READ, Client;
    "subscribeTerminalMetadata" => Stream, TERMINAL_READ, Client;
    "subscribeVcsBranch" => Stream, ORCHESTRATION_READ, Kept(kept::VERSION_CONTROL);
    "subscribeVcsStatus" => Stream, ORCHESTRATION_READ, Client;
    "subscribeWorktreeSetup" => Stream, ORCHESTRATION_READ, Kept(kept::WORKTREE_SETUP);
    "terminal.attach" => Stream, TERMINAL_OPERATE, Client;
    "terminal.clear" => Unary, TERMINAL_OPERATE, Client;
    "terminal.close" => Unary, TERMINAL_OPERATE, Client;
    "terminal.observe" => Stream, TERMINAL_READ, Client;
    "terminal.open" => Unary, TERMINAL_OPERATE, Kept(kept::SINGLE_CALLS);
    "terminal.resize" => Unary, TERMINAL_OPERATE, Client;
    "terminal.write" => Unary, TERMINAL_OPERATE, Client;
    "vcs.createRef" => Unary, SOURCE_CONTROL_WRITE, Client;
    "vcs.createWorktree" => Unary, SOURCE_CONTROL_WRITE, Client;
    "vcs.init" => Unary, SOURCE_CONTROL_WRITE, Kept(kept::VERSION_CONTROL);
    "vcs.listRefs" => Unary, ORCHESTRATION_READ, Client;
    "vcs.pull" => Unary, SOURCE_CONTROL_WRITE, Client;
    "vcs.refreshStatus" => Unary, ORCHESTRATION_READ, Client;
    "vcs.removeWorktree" => Unary, SOURCE_CONTROL_WRITE, Client;
    "vcs.switchRef" => Unary, SOURCE_CONTROL_WRITE, Client;
    "worktreeSetup.cancel" => Unary, ORCHESTRATION_OPERATE, Kept(kept::WORKTREE_SETUP);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_in_order_and_found_by_name() {
        assert!(METHODS.windows(2).all(|pair| pair[0].name < pair[1].name), "the list is sorted by name, each name once");
        for listed in METHODS {
            assert_eq!(method(listed.name), Some(listed));
        }
        assert_eq!(method("no.such.method"), None);
    }
}
