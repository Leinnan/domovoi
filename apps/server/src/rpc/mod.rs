//! WebSocket RPC handlers (t3code `WsRpcGroup` subset).

mod access;
pub(crate) mod admin_failure;
mod artifacts;
mod connect;
mod agent_sessions;
mod fs;
mod git_ops;
pub(crate) mod issues;
pub(crate) mod orchestration;
pub(crate) mod projects;
mod local_servers;
pub(crate) mod preview;
mod provider;
mod scheduled_tasks;
mod server;
mod server_admin;
pub(crate) mod shell;
mod telemetry;
pub(crate) mod source_control;
mod terminal;
mod vcs;

use crate::state::ConnCtx;
use effect_rpc::Router;
use futures::stream::{BoxStream, StreamExt};

/// Registers every implemented method.
pub fn router() -> Router<ConnCtx> {
    let r = Router::new().guard(|ctx: &ConnCtx, meta, payload| {
        crate::permissions::check(&meta.tag, payload, &ctx.session.permissions).map_err(|refused| effect_rpc::Failure::fail(&refused))
    });
    // Reading the diagnostics would otherwise trace itself (t3code's `RPC_METHODS_WITH_TRACING_DISABLED`).
    let r = r.untraced(&["server.getTraceDiagnostics", "server.getProcessDiagnostics", "server.getProcessResourceHistory", "server.signalProcess"]);
    let r = server::register(r);
    let r = shell::register(r);
    let r = fs::register(r);
    let r = terminal::register(r);
    let r = orchestration::register(r);
    let r = projects::register(r);
    let r = git_ops::register(r);
    let r = preview::register(r);
    let r = local_servers::register(r);
    let r = access::register(r);
    let r = telemetry::register(r);
    let r = connect::register(r);
    let r = server_admin::register(r);
    let r = scheduled_tasks::register(r);
    let r = provider::register(r);
    let r = agent_sessions::register(r);
    let r = source_control::register(r);
    let r = artifacts::register(r);
    let r = issues::register(r);
    vcs::register(r)
}

/// The failure of a call that cannot fail, and the element of a list that is
/// always empty: a type with no value.
#[derive(Debug, Clone, Copy)]
pub enum Never {}

impl serde::Serialize for Never {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        match *self {}
    }
}

/// Emits `items`, then stays open until the client interrupts (subscriptions
/// that have no further updates yet).
pub fn then_pending<T: Send + 'static, E: Send + 'static>(items: Vec<T>) -> BoxStream<'static, Result<T, E>> {
    futures::stream::iter(items.into_iter().map(Ok)).chain(futures::stream::pending()).boxed()
}

pub use shell::snapshot_off_runtime as shell_snapshot;

/// Methods that are registered though no client of this repository calls
/// them: features the server implements and no app screen uses yet. Each
/// stays until it gets a screen or is taken out; a method that is neither
/// called by the client core nor listed here fails
/// `every_registered_method_has_a_caller_or_is_kept_on_purpose`.
#[cfg(test)]
const KEPT_WITHOUT_A_CALLER: &[(&str, &[&str])] = &[
    (
        "issues from GitHub, GitLab and Linear (the agent tools read them through the same service)",
        &[
            "issueTrackers.bindProject", "issueTrackers.connect", "issueTrackers.list", "issues.candidates",
            "issues.comment", "issues.comments", "issues.create", "issues.detail",
            "issues.linkedThreads", "issues.list", "issues.react", "issues.setState",
            "issues.templates", "issues.update",
        ],
    ),
    (
        "the rest of a pull request's page: detail, review threads, labels, reviewers, stacks, review guides",
        &[
            "pullRequests.activity", "pullRequests.changeBreakdown", "pullRequests.detail", "pullRequests.diffFileContents",
            "pullRequests.filesViewed", "pullRequests.filterOptions", "pullRequests.generateReviewGuide", "pullRequests.invalidate",
            "pullRequests.labelCandidates", "pullRequests.linkedThreads", "pullRequests.listStats", "pullRequests.preview",
            "pullRequests.replyToThread", "pullRequests.reportState", "pullRequests.requestReviewers", "pullRequests.reviewGuide",
            "pullRequests.reviewerCandidates", "pullRequests.routing", "pullRequests.routingIdentity", "pullRequests.setFilesViewed",
            "pullRequests.setLabels", "pullRequests.setReaction", "pullRequests.setThreadResolution", "pullRequests.stack",
            "pullRequests.subscribeLinkedThreadRefreshes", "pullRequests.subscribeRefreshes", "pullRequests.threadComments", "pullRequests.update",
            "pullRequests.updateComment",
        ],
    ),
    (
        "repositories: sign-in to a host, lookup, clone and publish",
        &[
            "projectClone.cancel", "projectClone.retry", "projectClone.start", "sourceControl.appAccess",
            "sourceControl.cloneRepository", "sourceControl.lookupRepository", "sourceControl.publishRepository", "sourceControl.signInCancel",
            "sourceControl.signInStart", "sourceControl.signInWait", "sourceControl.star", "sourceControl.starred",
            "subscribeProjectClones",
        ],
    ),
    (
        "the preview browser's tabs and the dev servers found on this machine (the agent tools drive the same browser)",
        &[
            "preview.adjust", "preview.clearProfile", "preview.close", "preview.list",
            "preview.open", "preview.reportProfiles", "preview.reportStatus", "preview.resize",
            "preview.stopLocalServer", "subscribeDiscoveredLocalServers", "subscribePreviewEvents",
        ],
    ),
    (
        "resource telemetry over time, and signalling a process",
        &[
            "server.getProcessResourceHistory", "server.getResourceTelemetryHistory", "server.retryResourceTelemetry", "server.signalProcess",
            "subscribeResourceTelemetry",
        ],
    ),
    (
        "the trace file's diagnostics",
        &[
            "server.checkOtlpEndpoint", "server.getTraceDiagnostics",
        ],
    ),
    (
        "linked servers (the agent tools list them)",
        &[
            "peerLinks.link", "peerLinks.list", "peerLinks.unlink",
        ],
    ),
    (
        "the ACP registry: search, install, uninstall, and an agent's sign-in by URL",
        &[
            "server.acceptAcpRegistryUrlAuth", "server.prepareAcpRegistryAgent", "server.searchAcpRegistry", "server.uninstallAcpRegistryManagedBinary",
        ],
    ),
    (
        "workflows, native child work and a command's live output",
        &[
            "orchestration.getNativeChildWork", "orchestration.getWorkflowScript", "orchestration.stopWorkflow", "orchestration.subscribeCommandOutput",
        ],
    ),
    (
        "reading parts of a thread without subscribing to it",
        &[
            "orchestration.getThreadFamily", "orchestration.getThreadLinkMetadata", "orchestration.getThreadProjection", "orchestration.getThreadTitles",
            "orchestration.getTurnItem", "orchestration.getWorktreeThreadIds",
        ],
    ),
    (
        "a thread's artifacts",
        &[
            "artifacts.list", "artifacts.read",
        ],
    ),
    (
        "a worktree's setup script",
        &[
            "subscribeWorktreeSetup", "worktreeSetup.cancel",
        ],
    ),
    (
        "files: ignored files, a path's metadata, and attachments uploaded over HTTP (the agent tools hand out the same upload URL)",
        &[
            "attachments.createUploadUrl", "attachments.delete", "filesystem.getMetadata", "projects.listIgnoredFiles",
        ],
    ),
    (
        "version control: a new repository, the branch alone, a branch's commits, a diff's files, a pull request by its URL",
        &[
            "git.resolvePullRequest", "review.getBranchCommits", "review.getDiffFileContents", "server.discoverSourceControl",
            "subscribeVcsBranch", "vcs.init",
        ],
    ),
    (
        "administration: the configuration read once (the API cases read it back after a change), a keybinding added, text generator models, issue triage, usage rates, a provider CLI's update, a secret source checked",
        &[
            "server.checkSecretSource", "server.getConfig", "server.listTextGeneratorModels", "server.prepareTriage",
            "server.refreshUsageRates", "server.updateProvider", "server.upsertKeybinding",
        ],
    ),
    (
        "a terminal opened by itself, one webhook delivery, signing a provider instance out",
        &[
            "provider.auth.logout", "scheduledTasks.getWebhookDelivery", "terminal.open",
        ],
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// The methods the client core calls, read from where it builds its
    /// requests (`apps/domovoi-core/src/requests.rs`): the literal given to
    /// `built(`, `bare(` or `method:`, and a method named by a constant.
    fn called_by_the_client_core() -> BTreeSet<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../domovoi-core/src/requests.rs");
        let source = std::fs::read_to_string(path).expect("the client core's requests");
        let source = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut methods = BTreeSet::new();
        for opening in ["built(\"", "bare(\"", "method: \"", ": &str = \""] {
            for after in source.split(opening).skip(1) {
                if let Some((method, _)) = after.split_once('"') {
                    methods.insert(method.to_owned());
                }
            }
        }
        assert!(methods.len() > 50, "the client core's requests were not read: {methods:?}");
        methods
    }

    #[test]
    fn every_method_the_client_core_calls_is_registered() {
        let router = super::router();
        let missing: Vec<String> = called_by_the_client_core().into_iter().filter(|method| !router.has(method)).collect();
        assert!(missing.is_empty(), "the client core calls methods the server does not have: {missing:?}");
    }

    #[test]
    fn every_registered_method_has_a_caller_or_is_kept_on_purpose() {
        let router = super::router();
        let called = called_by_the_client_core();
        let kept: BTreeSet<&str> = super::KEPT_WITHOUT_A_CALLER.iter().flat_map(|(_, methods)| methods.iter().copied()).collect();
        let registered: BTreeSet<&str> = router.tags().collect();
        let unaccounted: Vec<&str> = registered.iter().copied().filter(|method| !called.contains(*method) && !kept.contains(method)).collect();
        assert!(unaccounted.is_empty(), "registered, not called by the client core and not kept on purpose ({}): {unaccounted:?}", unaccounted.len());
        let stale: Vec<&str> = kept.iter().copied().filter(|method| !registered.contains(method) || called.contains(*method)).collect();
        assert!(stale.is_empty(), "kept on purpose, but gone or called by now: {stale:?}");
    }
}
