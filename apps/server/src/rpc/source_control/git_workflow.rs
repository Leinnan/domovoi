//! `git.resolvePullRequest`, `git.preparePullRequestThread` and
//! `git.runStackedAction`, after t3code's `GitManager`.
//!
//! The source control provider of a checkout comes from its primary remote
//! (`origin`, else the first). GitHub is served through `gh`; a pull request
//! is also made on GitLab (gitlab.com, or a host whose name says it is one)
//! through `glab`; other hosts fail as t3code's unregistered providers do.
//!
//! `git.preparePullRequest` answers what a pull request from a checkout
//! would be, for a sheet a person reads before `git.runStackedAction` makes
//! it with the title, body, base and draft state they left (`pullRequest`).
//! With a `threadId` the pull request is linked to the thread (`created`),
//! and the issue the thread was started on is closed by a line in the body.
//!
//! Differences from t3code:
//! - commit messages and pull request text come from the local text
//!   generation CLI (`crate::text_gen`); a pull request whose text can't be
//!   written takes it from its commits (`gh pr create --fill`);
//! - commit hooks run, but their output is not streamed as `hook_*` events;
//! - the project's worktree setup script is not run for a new pull request
//!   worktree;
//! - checking out a pull request in a worktree fetches GitHub's
//!   `refs/pull/<n>/head` into the local branch and adds a worktree; t3code's
//!   handling of reused worktrees and rewritten heads is not reproduced.

#![expect(clippy::disallowed_types, reason = "what `gh pr view` and `gh pr list` printed, read into private records")]

use crate::gh;
use crate::orchestration::commands::Orchestrator;
use crate::state::{AppState, ConnCtx};
use crate::text_gen::TextGenerationError;
use crate::vcs_ops::GitCommandError;
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use domovoi_model::typed::{
    ServerSettings, SourceControlWritingStyle, SourceControlWritingStyleMode, SourceControlWritingStyleSettings,
    GitActionFailed, GitActionFinished, GitActionPhaseStarted, GitActionProgressEvent, GitActionProgressPhase, GitActionStarted, GitActionToast,
    GitActionToastCta, GitActionToastNoCta, GitActionToastOpenPr, GitActionToastRun, GitActionToastRunAction, GitBranchStep, GitBranchStepStatus,
    GitCommitStep, GitCommitStepStatus, GitPrStep, GitPrStepStatus, GitPreparePullRequestThreadResult, GitPullRequestState, GitPushStep,
    GitPushStepStatus, GitResolvePullRequestResult, GitResolvedPullRequest, GitRunStackedActionResult, GitStackedAction,
    PullRequestClosingRef, PullRequestDirtyFile, PullRequestExisting, PullRequestLinkSource, PullRequestPrepareInput, PullRequestPrepareResult,
    PullRequestUnavailable, PullRequestWatchPolicy, PullRequestWatchUntil, StackedPullRequest, ThreadIssueLink,
};
use domovoi_model::typed::command::{PullRequestToLink, ThreadPullRequestLink, ThreadPullRequestWatch};

// ---------------------------------------------------------------------------
// failures
// ---------------------------------------------------------------------------

/// Why a `git.*` call failed, as the client is told: t3code's
/// `GitManagerServiceError`, each with the fields this server gives it.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    /// The git workflow refused, or could not go on.
    #[error("{detail}")]
    #[serde(rename = "GitManagerError")]
    Manager { operation: &'static str, cwd: String, detail: String },

    /// The checkout's source control provider is not served, or its CLI failed.
    #[error("{detail}")]
    #[serde(rename = "SourceControlProviderError")]
    Provider {
        /// The provider kind (`github`, `gitlab`, `bitbucket`), or `unknown`.
        provider: &'static str,
        operation: &'static str,
        cwd: String,
        detail: String,
        /// The pull request or branch asked about, once one is.
        #[serde(skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
    },

    /// A pull request's head could not be fetched into a local branch.
    #[error("Failed to materialize pull request #{pull_request_number} branch {head_branch} as {local_branch}.")]
    #[serde(rename = "GitPullRequestMaterializationError", rename_all = "camelCase")]
    Materialization {
        cwd: String,
        pull_request_number: u64,
        /// `owner/name` of the repository the head is in, when `gh` named it.
        head_repository: Option<String>,
        head_branch: String,
        local_branch: String,
        cause: Box<GitCommandError>,
    },

    #[error(transparent)]
    #[serde(untagged)]
    Git(#[from] GitCommandError),

    #[error(transparent)]
    #[serde(untagged)]
    TextGeneration(#[from] TextGenerationError),
}

impl Failure {
    /// What a failed git action says went wrong: the failure's `detail`.
    fn detail(&self) -> Option<&str> {
        match self {
            Failure::Manager { detail, .. } | Failure::Provider { detail, .. } => Some(detail),
            Failure::Git(e) => Some(&e.detail),
            Failure::TextGeneration(e) => Some(&e.detail),
            Failure::Materialization { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// git helpers
// ---------------------------------------------------------------------------

struct GitOut {
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

async fn git(cwd: &str, args: &[&str]) -> GitOut {
    let mut cmd = crate::process::git(cwd);
    cmd.args(args);
    match crate::process::output(cmd, crate::process::SLOW).await {
        Ok(o) => GitOut {
            ok: o.status.success(),
            code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => GitOut { ok: false, code: None, stdout: String::new(), stderr: e.to_string() },
    }
}

fn command_error(operation: &str, cwd: &str, args: &[&str], detail: impl Into<String>, code: Option<i32>) -> GitCommandError {
    GitCommandError {
        operation: operation.to_owned(),
        command: format!("git {}", args.first().unwrap_or(&"")),
        cwd: cwd.to_owned(),
        detail: detail.into(),
        argument_count: Some(args.len()),
        exit_code: code,
    }
}

/// Runs git, mapping failure to `GitCommandError`.
async fn git_ok(operation: &str, cwd: &str, args: &[&str]) -> Result<String, GitCommandError> {
    let out = git(cwd, args).await;
    if out.ok {
        Ok(out.stdout)
    } else {
        let detail = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git failed").to_owned();
        Err(command_error(operation, cwd, args, detail, out.code))
    }
}

fn manager_error(operation: &'static str, cwd: &str, detail: impl Into<String>) -> Failure {
    Failure::Manager { operation, cwd: cwd.to_owned(), detail: detail.into() }
}

fn provider_error(provider: &'static str, operation: &'static str, cwd: &str, reference: Option<&str>, detail: impl Into<String>) -> Failure {
    Failure::Provider { provider, operation, cwd: cwd.to_owned(), detail: detail.into(), reference: reference.map(str::to_owned) }
}

/// `statusDetails`: branch, upstream and ahead/behind counts.
struct Status {
    branch: Option<String>,
    upstream_ref: Option<String>,
    ahead: u64,
    behind: u64,
}

async fn status_details(cwd: &str) -> Result<Status, GitCommandError> {
    let args = ["rev-parse", "--is-inside-work-tree"];
    if !Path::new(cwd).is_dir() || git(cwd, &args).await.stdout.trim() != "true" {
        return Err(command_error("GitVcsDriver.statusDetails", cwd, &args, format!("{cwd} is not a git repository."), None));
    }
    let branch = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await;
    let branch = branch.ok.then(|| branch.stdout.trim().to_owned()).filter(|b| !b.is_empty());
    let upstream = git(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).await;
    let upstream_ref = upstream.ok.then(|| upstream.stdout.trim().to_owned()).filter(|u| !u.is_empty());
    let (mut ahead, mut behind) = (0, 0);
    if upstream_ref.is_some() {
        let counts = git(cwd, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"]).await.stdout;
        let mut it = counts.split_whitespace();
        ahead = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        behind = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    }
    Ok(Status { branch, upstream_ref, ahead, behind })
}

async fn primary_remote(cwd: &str) -> Option<(String, String)> {
    let remotes = git(cwd, &["remote"]).await.stdout;
    let name = remotes.lines().find(|r| *r == "origin").or_else(|| remotes.lines().next())?.to_owned();
    let url = git(cwd, &["remote", "get-url", &name]).await;
    url.ok.then(|| (name, url.stdout.trim().to_owned()))
}

/// The checkout's source control provider (`kind`, host, `owner/name`).
struct ProviderContext {
    kind: &'static str,
    host: String,
    repository: String,
}

fn provider_kind(host: &str) -> &'static str {
    match host {
        "github.com" => "github",
        "gitlab.com" => "gitlab",
        "bitbucket.org" => "bitbucket",
        // A self-managed GitLab, by its name.
        host if crate::glab::recognizes_host(host) => "gitlab",
        _ => "unknown",
    }
}

async fn detect_provider(cwd: &str) -> Result<Option<ProviderContext>, Failure> {
    let inside = git(cwd, &["rev-parse", "--is-inside-work-tree"]).await;
    if !Path::new(cwd).is_dir() || inside.stdout.trim() != "true" {
        return Err(provider_error("unknown", "detectProvider", cwd, None, "Failed to detect source control provider."));
    }
    let Some((_, url)) = primary_remote(cwd).await else { return Ok(None) };
    Ok(crate::vcs::parse_remote(&url).map(|(host, owner, name)| ProviderContext {
        kind: provider_kind(&host),
        host,
        repository: format!("{owner}/{name}"),
    }))
}

fn terms(kind: Option<&str>) -> (&'static str, &'static str, &'static str) {
    // (shortLabel, shortLabelWithArticle, singular)
    match kind {
        Some("github") => ("PR", "a PR", "pull request"),
        _ => ("change request", "a change request", "change request"),
    }
}

fn normalize_reference(reference: &str) -> String {
    let t = reference.trim();
    match t.strip_prefix('#') {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => n.to_owned(),
        _ => t.to_owned(),
    }
}

const PR_FIELDS: &str = "number,title,url,baseRefName,headRefName,state,mergedAt,headRefOid,isCrossRepository,headRepository,headRepositoryOwner";

/// What `gh pr view` says of a pull request, as far as it is used here.
struct ViewedPullRequest {
    number: u64,
    head_branch: String,
    /// The commit its head is at.
    head_sha: Option<String>,
    cross_repository: bool,
    /// `owner/name` of the repository the head is in.
    head_repository: Option<String>,
    resolved: GitResolvedPullRequest,
}

impl ViewedPullRequest {
    /// Reads `gh`'s JSON; a key it left out reads as empty.
    fn of(raw: &Value) -> Self {
        let text = |key: &str| raw[key].as_str().unwrap_or("").to_owned();
        let number = raw["number"].as_u64().unwrap_or(0);
        ViewedPullRequest {
            number,
            head_branch: text("headRefName"),
            head_sha: raw["headRefOid"].as_str().map(str::to_owned),
            cross_repository: raw["isCrossRepository"].as_bool().unwrap_or(false),
            head_repository: match (raw["headRepositoryOwner"]["login"].as_str(), raw["headRepository"]["name"].as_str()) {
                (Some(owner), Some(name)) => Some(format!("{owner}/{name}")),
                _ => None,
            },
            resolved: GitResolvedPullRequest {
                number,
                title: text("title"),
                url: text("url"),
                base_branch: text("baseRefName"),
                head_branch: text("headRefName"),
                state: GitPullRequestState::from(gh::state(raw["state"].as_str(), raw["mergedAt"].as_str()).as_str()),
            },
        }
    }
}

/// The GitHub pull request `reference` names in the checkout at `cwd`.
async fn resolve_change_request(cwd: &str, reference: &str) -> Result<(ProviderContext, ViewedPullRequest), Failure> {
    let ctx = match detect_provider(cwd).await? {
        Some(ctx) if ctx.kind == "github" => ctx,
        other => {
            let kind = other.map(|c| c.kind).unwrap_or("unknown");
            let detail = format!("No {kind} source control provider is registered.");
            return Err(provider_error(kind, "getChangeRequest", cwd, Some(reference), detail));
        }
    };
    // What `gh pr view` printed, read into a record by `ViewedPullRequest::of`.
    let raw = gh::run_json::<Value>(Some(cwd), &ctx.host, &["pr", "view", reference, "--repo", &ctx.repository, "--json", PR_FIELDS])
        .await
        .map_err(|e| provider_error("github", "getChangeRequest", cwd, Some(reference), e.detail))?;
    Ok((ctx, ViewedPullRequest::of(&raw)))
}

// ---------------------------------------------------------------------------
// resolvePullRequest / preparePullRequestThread
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ResolveInput {
    cwd: String,
    reference: String,
}

async fn resolve_pull_request(input: ResolveInput) -> Result<GitResolvePullRequestResult, Failure> {
    let (_, pr) = resolve_change_request(&input.cwd, &normalize_reference(&input.reference)).await?;
    Ok(GitResolvePullRequestResult { pull_request: pr.resolved })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareInput {
    cwd: String,
    reference: String,
    mode: String,
}

/// t3code's `sanitizeBranchFragment` (ASCII).
pub fn sanitize_branch_fragment(raw: &str) -> String {
    let trim_set = |c: char| matches!(c, '.' | '/' | '_' | '-') || c.is_whitespace();
    let lowered: String = raw.trim().to_lowercase().chars().filter(|c| !matches!(c, '\'' | '"' | '`')).collect();
    let lowered = lowered.trim_matches(trim_set);
    let mut out = String::new();
    for c in lowered.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '_' | '-') { c } else { '-' };
        let last = out.chars().last();
        if (mapped == '-' && last == Some('-')) || (mapped == '/' && last == Some('/')) {
            continue;
        }
        out.push(mapped);
    }
    let edge = |c: char| matches!(c, '.' | '/' | '_' | '-');
    let out: String = out.trim_matches(edge).chars().take(64).collect();
    let out = out.trim_end_matches(edge).to_owned();
    if out.is_empty() { "update".into() } else { out }
}

fn worktree_local_branch(number: u64, head_branch: &str, cross_repository: bool, local: &[String]) -> String {
    if !cross_repository {
        return head_branch.to_owned();
    }
    let suffix = sanitize_branch_fragment(head_branch);
    let creatable = |name: &str| {
        let segments: Vec<&str> = name.split('/').collect();
        let parent_taken = (1..segments.len()).any(|i| local.iter().any(|b| *b == segments[..i].join("/")));
        !parent_taken && !local.iter().any(|b| b.starts_with(&format!("{name}/")))
    };
    let preferred = format!("domovoi/pr-{number}/{suffix}");
    if creatable(&preferred) {
        return preferred;
    }
    let flat = format!("domovoi-pr-{number}-{}", suffix.replace('/', "-"));
    let mut attempt = 1;
    loop {
        let candidate = if attempt == 1 { flat.clone() } else { format!("{flat}-{attempt}") };
        if creatable(&candidate) && !local.contains(&candidate) {
            return candidate;
        }
        attempt += 1;
    }
}

async fn head_sha(cwd: &str) -> Option<String> {
    let out = git(cwd, &["rev-parse", "HEAD"]).await;
    out.ok.then(|| out.stdout.trim().to_owned())
}

/// t3code's worktree folder: `<worktrees>/<repo>/<branch prefix>-<hash>`.
async fn worktree_path(state: &AppState, cwd: &str, branch: &str) -> std::path::PathBuf {
    let setting = state.settings.read().await.worktrees_directory.clone().unwrap_or_default();
    let parent = if setting.is_empty() { state.paths.home.join("worktrees") } else { std::path::PathBuf::from(setting) };
    let repo_name = Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let common = git(cwd, &["rev-parse", "--git-common-dir"]).await.stdout.trim().to_owned();
    let hash = crate::util::sha256_hex(format!("{common}\0{branch}").as_bytes());
    let mut prefix = String::new();
    for c in branch.replace('/', "-").chars() {
        if prefix.len() + c.len_utf8() > 23 {
            break;
        }
        prefix.push(c);
    }
    let base = parent.join(repo_name).join(format!("{prefix}-{}", &hash[..16]));
    let mut path = base.clone();
    let mut n = 2;
    while path.exists() {
        path = std::path::PathBuf::from(format!("{}-{n}", base.display()));
        n += 1;
    }
    path
}

async fn prepare_pull_request_thread(state: Arc<AppState>, input: PrepareInput) -> Result<GitPreparePullRequestThreadResult, Failure> {
    let reference = normalize_reference(&input.reference);
    let cwd = input.cwd.as_str();
    let (ctx, pr) = resolve_change_request(cwd, &reference).await?;
    let ViewedPullRequest { number, head_branch, head_sha: pr_head, cross_repository: cross, head_repository, resolved: pull_request } = pr;
    let prepared = |branch: String, worktree_path: Option<String>, on_head: bool| GitPreparePullRequestThreadResult {
        pull_request: pull_request.clone(),
        branch,
        worktree_path,
        is_on_pull_request_head: Some(on_head),
    };
    if input.mode == "local" {
        let tracked = git_ok("GitManager.preparePullRequestThread.trackedChanges", cwd, &["status", "--porcelain", "--untracked-files=no"]).await?;
        if !tracked.trim().is_empty() {
            return Err(manager_error(
                "preparePullRequestThread",
                cwd,
                format!("This repository has uncommitted changes. Commit or stash them before checking out pull request #{number} here, or check it out in its own folder."),
            ));
        }
        gh::run(Some(cwd), &ctx.host, &["pr", "checkout", &number.to_string()], None)
            .await
            .map_err(|e| provider_error("github", "checkoutChangeRequest", cwd, Some(&reference), e.detail))?;
        let status = status_details(cwd).await?;
        let on_head = pr_head.is_some() && head_sha(cwd).await == pr_head;
        return Ok(prepared(status.branch.unwrap_or(head_branch), None, on_head));
    }
    let local: Vec<String> = git(cwd, &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"])
        .await
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let branch = worktree_local_branch(number, &head_branch, cross, &local);
    // A worktree that already has the branch checked out is reused.
    let listing = git(cwd, &["worktree", "list", "--porcelain"]).await.stdout;
    let mut current: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(p.to_owned());
        } else if line.strip_prefix("branch refs/heads/") == Some(branch.as_str()) {
            if let Some(path) = current.clone().filter(|p| Path::new(p) != Path::new(cwd)) {
                let on_head = pr_head.is_some() && head_sha(&path).await == pr_head;
                return Ok(prepared(branch, Some(path), on_head));
            }
        }
    }
    if !local.contains(&branch) {
        let (remote, _) = primary_remote(cwd).await.ok_or_else(|| manager_error("preparePullRequestThread", cwd, "This repository has no remote."))?;
        let refspec = format!("refs/pull/{number}/head:refs/heads/{branch}");
        git_ok("GitManager.preparePullRequestThread.fetch", cwd, &["fetch", "--", &remote, &refspec]).await.map_err(|cause| Failure::Materialization {
            cwd: cwd.to_owned(),
            pull_request_number: number,
            head_repository,
            head_branch: head_branch.clone(),
            local_branch: branch.clone(),
            cause: Box::new(cause),
        })?;
    }
    let path = worktree_path(&state, cwd, &branch).await;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let path_str = path.to_string_lossy().into_owned();
    git_ok("GitVcsDriver.createWorktree", cwd, &["worktree", "add", &path_str, &branch]).await?;
    let on_head = pr_head.is_some() && head_sha(&path_str).await == pr_head;
    Ok(prepared(branch, Some(path_str), on_head))
}

// ---------------------------------------------------------------------------
// runStackedAction
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StackedInput {
    action_id: String,
    cwd: String,
    action: String,
    #[serde(default)]
    commit_message: Option<String>,
    #[serde(default)]
    feature_branch: Option<bool>,
    #[serde(default)]
    file_paths: Option<Vec<String>>,
    /// The thread the work is of: the pull request is linked to it.
    #[serde(default)]
    thread_id: Option<String>,
    /// The pull request as the sheet's reader left it; without one its text is generated.
    #[serde(default)]
    pull_request: Option<StackedPullRequest>,
}

fn is_commit_action(action: &str) -> bool {
    matches!(action, "commit" | "commit_push" | "commit_push_pr")
}

fn parse_commit_message(raw: &str) -> Option<(String, String)> {
    let normalized = raw.replace("\r\n", "\n");
    let normalized = normalized.trim();
    let (first, rest) = normalized.split_once('\n').unwrap_or((normalized, ""));
    let subject = first.trim();
    (!subject.is_empty()).then(|| (subject.to_owned(), rest.trim().to_owned()))
}

/// The server's settings.
async fn writer_settings() -> crate::settings::Settings {
    match crate::state::STATE.get() {
        Some(s) => s.settings.read().await.clone(),
        None => Default::default(),
    }
}

/// The `sourceControlWritingStyle` setting, when it is the settings record
/// (the instructions alone, as older servers stored them, set neither a mode
/// nor a rule about templates).
fn writing_style(settings: &ServerSettings) -> Option<&SourceControlWritingStyleSettings> {
    match settings.source_control_writing_style.as_ref()? {
        SourceControlWritingStyle::Settings(style) => Some(style),
        SourceControlWritingStyle::Legacy(_) => None,
    }
}

/// The user's own writing rules, when `sourceControlWritingStyle` has them.
fn writing_instructions(settings: &ServerSettings) -> Option<String> {
    let style = writing_style(settings).filter(|style| style.mode == Some(SourceControlWritingStyleMode::Custom))?;
    style.custom_instructions.clone().filter(|s| !s.trim().is_empty())
}

/// A commit message for what is staged: `(subject, body, branch fragment)`.
async fn generate_commit_message(cwd: &str, branch: Option<&str>, paths: Option<&[String]>, with_branch: bool) -> Result<(String, String, Option<String>), TextGenerationError> {
    let mut stat = vec!["diff", "--cached", "--stat", "--"];
    let mut patch = vec!["diff", "--cached", "--no-color", "--"];
    if let Some(p) = paths {
        stat.extend(p.iter().map(String::as_str));
        patch.extend(p.iter().map(String::as_str));
    }
    let settings = writer_settings().await;
    let prompt = crate::text_gen::commit_message_prompt(branch, &git(cwd, &stat).await.stdout, &git(cwd, &patch).await.stdout, with_branch, writing_instructions(&settings).as_deref());
    let answer = crate::text_gen::generate(crate::text_gen::Request {
        operation: "generateCommitMessage", cwd: Some(cwd), prompt, model: crate::text_gen::model_for(&settings, crate::text_gen::Writer::SourceControl), investigate: false,
    }).await?;
    let subject = answer.subject.as_deref().map(|s| s.lines().next().unwrap_or("").trim().trim_end_matches('.').to_owned()).filter(|s| !s.is_empty())
        .ok_or_else(|| crate::text_gen::error("generateCommitMessage", "The model returned no subject."))?;
    let body = answer.body.as_deref().unwrap_or_default().trim().to_owned();
    Ok((subject, body, answer.branch))
}

/// What a pull request's text is written from.
struct PrSource<'a> {
    base: &'a str,
    head: &'a str,
    /// Count what is not committed yet too: the sheet is read before the commit is made.
    uncommitted: bool,
    /// The issue the change is the work on, as `#12 Title`.
    issue: Option<&'a str>,
}

/// `base` as this checkout knows the remote's: `origin/main`, else the local branch.
async fn base_ref_of(cwd: &str, base: &str) -> String {
    let remote = primary_remote(cwd).await.map_or_else(|| "origin".to_owned(), |(name, _)| name);
    let remote_base = format!("{remote}/{base}");
    if git(cwd, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{remote_base}")]).await.ok { remote_base } else { base.to_owned() }
}

/// A pull request title and body from the branch's commits and diff.
async fn generate_pr_content(cwd: &str, source: PrSource<'_>) -> Result<(String, String), TextGenerationError> {
    let PrSource { base, head, uncommitted, issue } = source;
    let base_ref = base_ref_of(cwd, base).await;
    let range = format!("{base_ref}...HEAD");
    let commits = git(cwd, &["log", "--format=- %s%n%b", &format!("{base_ref}..HEAD")]).await.stdout;
    let fork_point = git(cwd, &["merge-base", &base_ref, "HEAD"]).await;
    let (stat, patch) = match fork_point.ok.then(|| fork_point.stdout.trim().to_owned()).filter(|point| uncommitted && !point.is_empty()) {
        // From where the branch left its base to the files as they are now.
        Some(point) => {
            let mut stat = git(cwd, &["diff", "--stat", &point]).await.stdout;
            let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard"]).await.stdout;
            if !untracked.trim().is_empty() {
                stat.push_str(&format!("\nNew files, not yet added:\n{}", untracked.trim_end()));
            }
            (stat, git(cwd, &["diff", "--no-color", &point]).await.stdout)
        }
        None => (git(cwd, &["diff", "--stat", &range]).await.stdout, git(cwd, &["diff", "--no-color", &range]).await.stdout),
    };
    let settings = writer_settings().await;
    let template = (writing_style(&settings).and_then(|style| style.follow_change_request_templates) != Some(false)).then(|| {
        [".github/pull_request_template.md", ".github/PULL_REQUEST_TEMPLATE.md", "docs/pull_request_template.md", "PULL_REQUEST_TEMPLATE.md", "pull_request_template.md"]
            .iter().find_map(|p| std::fs::read_to_string(Path::new(cwd).join(p)).ok())
    }).flatten();
    let prompt = crate::text_gen::pr_content_prompt(base, head, &commits, &stat, &patch, template.as_deref(), writing_instructions(&settings).as_deref(), issue);
    let answer = crate::text_gen::generate(crate::text_gen::Request {
        operation: "generatePrContent", cwd: Some(cwd), prompt, model: crate::text_gen::model_for(&settings, crate::text_gen::Writer::SourceControl), investigate: false,
    }).await?;
    let title = answer.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| crate::text_gen::error("generatePrContent", "The model returned no title."))?;
    Ok((title.to_owned(), answer.body.as_deref().unwrap_or_default().trim().to_owned()))
}

/// Stages the commit's paths; false when there is nothing to commit.
async fn stage(cwd: &str, file_paths: Option<&[String]>) -> Result<bool, GitCommandError> {
    let mut add = vec!["add", "-A", "--"];
    let mut check = vec!["diff", "--cached", "--quiet", "--"];
    if let Some(paths) = file_paths {
        add.extend(paths.iter().map(String::as_str));
        check.extend(paths.iter().map(String::as_str));
    }
    git_ok("GitVcsDriver.prepareCommitContext", cwd, &add).await?;
    let has_head = git(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.ok;
    if !has_head {
        return Ok(!git(cwd, &["ls-files", "--cached"]).await.stdout.trim().is_empty());
    }
    Ok(git(cwd, &check).await.code == Some(1))
}

fn sanitize_feature_branch(raw: &str) -> String {
    let s = sanitize_branch_fragment(raw);
    if s.contains('/') { s } else { format!("feature/{s}") }
}

fn resolve_feature_branch(existing: &[String], preferred: &str) -> String {
    let base = sanitize_feature_branch(if preferred.trim().is_empty() { "feature/update" } else { preferred });
    let names: Vec<String> = existing.iter().map(|b| b.to_lowercase()).collect();
    let parts: Vec<&str> = base.split('/').collect();
    let mut resolved = String::new();
    for (i, part) in parts.iter().enumerate() {
        let stem = if resolved.is_empty() { part.to_string() } else { format!("{resolved}/{part}") };
        let mut candidate = stem.clone();
        let mut suffix = 2;
        while names.contains(&candidate) || (i == parts.len() - 1 && names.iter().any(|n| n.starts_with(&format!("{candidate}/")))) {
            candidate = format!("{stem}-{suffix}");
            suffix += 1;
        }
        resolved = candidate;
    }
    resolved
}

fn short_sha(sha: Option<&str>) -> Option<String> {
    sha.map(|s| s.chars().take(7).collect())
}

fn truncate_text(v: Option<&str>) -> Option<String> {
    let v = v.filter(|s| !s.is_empty())?;
    if v.chars().count() <= 72 {
        return Some(v.to_owned());
    }
    Some(format!("{}...", v.chars().take(69).collect::<String>().trim_end()))
}

/// t3code's `summarizeGitActionResult` + `buildCompletionToast` (without the open-PR lookup).
fn completion_toast(action: &GitStackedAction, commit: &GitCommitStep, push: &GitPushStep, pr: &GitPrStep, short_label: &str, on_default_branch: bool) -> GitActionToast {
    let has_pr = matches!(pr.status, GitPrStepStatus::Created | GitPrStepStatus::OpenedExisting);
    let committed = commit.status == GitCommitStepStatus::Created;
    let pushed = push.status == GitPushStepStatus::Pushed;
    let (title, description) = if has_pr {
        let n = pr.number.map(|n| format!(" #{n}")).unwrap_or_default();
        let verb = if pr.status == GitPrStepStatus::Created { "Created" } else { "Opened" };
        (format!("{verb} {short_label}{n}"), truncate_text(pr.title.as_deref()))
    } else if pushed {
        let sha = short_sha(commit.commit_sha.as_deref()).map(|s| format!(" {s}")).unwrap_or_default();
        let target = push.upstream_branch.as_deref().or(push.branch.as_deref()).map(|b| format!(" to {b}")).unwrap_or_default();
        (format!("Pushed{sha}{target}"), truncate_text(commit.subject.as_deref()))
    } else if committed {
        let title = short_sha(commit.commit_sha.as_deref()).map(|s| format!("Committed {s}")).unwrap_or_else(|| "Committed changes".into());
        (title, truncate_text(commit.subject.as_deref()))
    } else {
        ("Done".to_owned(), None)
    };
    let open_pr = if has_pr { pr.url.as_deref() } else { None };
    let run = |label: String, kind: GitStackedAction| GitActionToastCta::RunAction(GitActionToastRun { label, action: GitActionToastRunAction { kind } });
    let cta = if *action == GitStackedAction::Commit && committed {
        run("Push".into(), GitStackedAction::Push)
    } else if let (false, Some(url)) = (*action == GitStackedAction::Commit, open_pr) {
        GitActionToastCta::OpenPr(GitActionToastOpenPr { label: format!("View {short_label}"), url: url.to_owned() })
    } else if matches!(action, GitStackedAction::Push | GitStackedAction::CommitPush) && pushed && !on_default_branch {
        run(format!("Create {short_label}"), GitStackedAction::CreatePr)
    } else {
        GitActionToastCta::None(GitActionToastNoCta {})
    };
    GitActionToast { title, description, cta }
}

async fn push_current_branch(cwd: &str, fallback: Option<&str>) -> Result<GitPushStep, GitCommandError> {
    let status = status_details(cwd).await?;
    let Some(branch) = status.branch.clone().or(fallback.map(str::to_owned)) else {
        return Err(command_error("GitVcsDriver.pushCurrentBranch", cwd, &["push"], "Cannot push from detached HEAD.", None));
    };
    if status.upstream_ref.is_some() && status.ahead == 0 && status.behind == 0 {
        return Ok(GitPushStep { status: GitPushStepStatus::SkippedUpToDate, branch: Some(branch), upstream_branch: status.upstream_ref, set_upstream: None });
    }
    if let Some(upstream) = status.upstream_ref {
        git_ok("GitVcsDriver.pushCurrentBranch", cwd, &["push"]).await?;
        return Ok(GitPushStep { status: GitPushStepStatus::Pushed, branch: Some(branch), upstream_branch: Some(upstream), set_upstream: Some(false) });
    }
    let Some((remote, _)) = primary_remote(cwd).await else {
        return Err(command_error(
            "GitVcsDriver.pushCurrentBranch", cwd, &["push"],
            "Cannot push because no git remote is configured for this repository.", None,
        ));
    };
    let refspec = format!("HEAD:refs/heads/{branch}");
    git_ok("GitVcsDriver.pushCurrentBranch", cwd, &["push", "-u", "--", &remote, &refspec]).await?;
    Ok(GitPushStep { status: GitPushStepStatus::Pushed, upstream_branch: Some(format!("{remote}/{branch}")), branch: Some(branch), set_upstream: Some(true) })
}

async fn default_branch(cwd: &str) -> Option<String> {
    let (remote, _) = primary_remote(cwd).await?;
    let out = git(cwd, &["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{remote}/HEAD")]).await;
    out.ok.then(|| out.stdout.trim().rsplit_once('/').map(|(_, b)| b.to_owned())).flatten()
}

/// An open pull request as `gh pr list` names it; a key it left out, or one
/// not of its type, is absent.
struct ListedPullRequest {
    number: Option<u64>,
    url: Option<String>,
    title: Option<String>,
    base_branch: Option<String>,
    head_branch: Option<String>,
}

impl ListedPullRequest {
    fn step(self, status: GitPrStepStatus) -> GitPrStep {
        GitPrStep { status, url: self.url, number: self.number, base_branch: self.base_branch, head_branch: self.head_branch, title: self.title }
    }
}

/// The branch's open pull request, or why the host could not be asked.
async fn open_change_request(ctx: &ProviderContext, cwd: &str, branch: &str) -> Result<Option<ListedPullRequest>, gh::GhError> {
    if ctx.kind == "gitlab" {
        let found = super::gitlab::find_open_merge_request(Some(cwd), &ctx.host, &ctx.repository, branch).await?;
        return Ok(found.map(ListedPullRequest::of_merge_request));
    }
    // What `gh pr list` printed, read into a record here.
    let raw = gh::run_json::<Value>(
        Some(cwd), &ctx.host,
        &["pr", "list", "--repo", &ctx.repository, "--head", branch, "--state", "open", "--limit", "1", "--json", "number,url,title,baseRefName,headRefName"],
    )
    .await?;
    let Some(first) = raw.as_array().and_then(|rows| rows.first()) else { return Ok(None) };
    let text = |key: &str| first[key].as_str().map(str::to_owned);
    Ok(Some(ListedPullRequest { number: first["number"].as_u64(), url: text("url"), title: text("title"), base_branch: text("baseRefName"), head_branch: text("headRefName") }))
}

async fn find_open_pr(ctx: &ProviderContext, cwd: &str, branch: &str) -> Option<ListedPullRequest> {
    open_change_request(ctx, cwd, branch).await.ok().flatten()
}

impl ListedPullRequest {
    fn of_merge_request(made: super::gitlab::MadeMergeRequest) -> Self {
        ListedPullRequest { number: Some(made.number), url: Some(made.url), title: Some(made.title), base_branch: made.base_branch, head_branch: made.head_branch }
    }
}

// ---------------------------------------------------------------------------
// The thread a pull request is made from
// ---------------------------------------------------------------------------

/// What a pull request takes from the thread it is made from.
#[derive(Default)]
struct ThreadContext {
    /// The branch the thread's worktree was cut from.
    base_branch: Option<String>,
    /// The issues the thread was started on.
    focus: Vec<ThreadIssueLink>,
    /// The pull requests it links already.
    linked: Vec<u64>,
}

fn thread_context(orchestrator: Option<&Orchestrator>, thread_id: Option<&str>) -> ThreadContext {
    let thread = orchestrator.zip(thread_id).and_then(|(orchestrator, id)| orchestrator.store().app_thread(id).ok().flatten());
    let Some(thread) = thread else { return ThreadContext::default() };
    ThreadContext {
        base_branch: thread.base_branch.clone(),
        focus: thread.issues.iter().flatten().filter(|link| link.is_focus()).cloned().collect(),
        linked: crate::orchestration::commands::pull_requests::links_of(&thread).iter().filter(|link| crate::orchestration::commands::pull_requests::is_visible(link)).map(|link| link.number).collect(),
    }
}

impl ThreadContext {
    /// The issue the work is on, as the text generation model is told: `#12 Title`.
    fn issue_hint(&self) -> Option<String> {
        self.focus.first().map(|link| format!("{} {}", link.display_key, link.title))
    }
}

/// The line that closes `issue` when a pull request in `repository` on
/// `host` merges. An issue of the same repository is `Closes #12`; one of
/// another repository of the host names it; Linear reads `Fixes ENG-12`; an
/// issue on another host is cited by its URL (nothing closes it by itself).
fn closing_reference(issue: &ThreadIssueLink, host: &str, repository: &str) -> PullRequestClosingRef {
    let key = &issue.key;
    let reference = if key.tracker == "linear" {
        format!("Fixes {}", issue.display_key)
    } else if !key.host.eq_ignore_ascii_case(host) {
        format!("Closes {}", issue.url)
    } else if key.scope.eq_ignore_ascii_case(repository) {
        format!("Closes #{}", key.key)
    } else {
        format!("Closes {}#{}", key.scope, key.key)
    };
    PullRequestClosingRef { display_key: issue.display_key.clone(), reference, url: issue.url.clone() }
}

/// `body` ending with the closing lines it does not have yet.
fn with_closing(body: &str, closing: &[PullRequestClosingRef]) -> String {
    let lower = body.to_lowercase();
    let missing: Vec<&str> = closing.iter().map(|c| c.reference.as_str()).filter(|line| !lower.contains(&line.to_lowercase())).collect();
    match (body.trim().is_empty(), missing.is_empty()) {
        (_, true) => body.trim().to_owned(),
        (true, false) => missing.join("\n"),
        (false, false) => format!("{}\n\n{}", body.trim(), missing.join("\n")),
    }
}

/// Links a pull request a git action made, or found open, to its thread,
/// and has the thread follow it until it merges: what it shows stays
/// current, and the agent is woken to fix checks that fail when the settings
/// say so (`prWatchWakeOnFailedChecks`, at most `prWatchMaxFixAttempts`
/// times in a row). A thread that cannot watch (it is settled, or a
/// subagent's) is only linked.
async fn link_to_thread(orchestrator: &Orchestrator, thread_id: &str, ctx: &ProviderContext, step: &GitPrStep) {
    let (Some(number), Some(url)) = (step.number, step.url.as_deref()) else { return };
    let source = if step.status == GitPrStepStatus::Created { PullRequestLinkSource::Created } else { PullRequestLinkSource::Manual };
    let repository_web_url = Some(format!("https://{}/{}", ctx.host, ctx.repository));
    let settings = writer_settings().await;
    let policy = PullRequestWatchPolicy::following(
        settings.pr_watch_wake_on_failed_checks != Some(false),
        settings.pr_watch_max_fix_attempts.as_ref().and_then(serde_json::Number::as_u64).unwrap_or(DEFAULT_FIX_ATTEMPTS),
    );
    let watch = ThreadPullRequestWatch {
        command_id: format!("git-action-pr-watch:{}", crate::util::new_uuid()).into(),
        thread_id: thread_id.into(),
        host: ctx.host.clone(),
        repository: ctx.repository.clone(),
        number,
        watching: true,
        until: Some(PullRequestWatchUntil::Merged),
        checks_pending: Some(true),
        link: Some(PullRequestToLink { url: url.to_owned(), source: source.clone(), repository_web_url: repository_web_url.clone() }),
        policy: Some(policy),
    };
    match orchestrator.dispatch(watch.into()).await {
        Ok(_) => crate::pr_watcher::wake_now(),
        Err(refused) => {
            tracing::info!(%thread_id, number, "the pull request is linked without a watch: {refused}");
            let link = ThreadPullRequestLink {
                command_id: format!("git-action-pr:{}", crate::util::new_uuid()).into(),
                thread_id: thread_id.into(),
                host: ctx.host.clone(),
                repository: ctx.repository.clone(),
                number,
                url: url.to_owned(),
                source,
                repository_web_url,
            };
            if let Err(e) = orchestrator.dispatch(link.into()).await {
                tracing::warn!(%thread_id, number, "the pull request was not linked to its thread: {e}");
            }
        }
    }
}

/// How many times in a row an agent is woken to fix a pull request's failed checks, unless the settings say.
const DEFAULT_FIX_ATTEMPTS: u64 = 3;

// ---------------------------------------------------------------------------
// preparePullRequest
// ---------------------------------------------------------------------------

/// The most changed files a sheet lists.
const MOST_DIRTY_FILES: usize = 200;
/// The most base branches it offers.
const MOST_BASE_BRANCHES: usize = 20;

/// `git.preparePullRequest`. What stands in the way of a pull request is
/// answered (`unavailable`), not failed: the sheet shows it.
async fn prepare_pull_request(orchestrator: &Orchestrator, input: PullRequestPrepareInput) -> Result<PullRequestPrepareResult, Failure> {
    let cwd = input.cwd.as_str();
    let mut out = PullRequestPrepareResult { provider: "unknown".into(), ..Default::default() };
    let unavailable = |mut out: PullRequestPrepareResult, reason: &str, detail: String| {
        out.unavailable = Some(PullRequestUnavailable { reason: reason.to_owned(), detail });
        Ok(out)
    };
    let Ok(status) = status_details(cwd).await else {
        return unavailable(out, "not-a-repository", "This folder is not a git repository.".into());
    };
    out.branch = status.branch.clone();
    out.has_upstream = status.upstream_ref.is_some();
    let files = crate::vcs::local_status(cwd).await.working_tree.files;
    out.dirty_truncated = files.len() > MOST_DIRTY_FILES;
    out.dirty_files = files.into_iter().take(MOST_DIRTY_FILES).map(|f| PullRequestDirtyFile { path: f.path, insertions: f.insertions, deletions: f.deletions }).collect();
    let thread = thread_context(Some(orchestrator), input.thread_id.as_ref().map(|id| id.as_str()));

    let Some((remote, _)) = primary_remote(cwd).await else {
        return unavailable(out, "no-remote", "This repository has no remote to push to.".into());
    };
    let Some(ctx) = detect_provider(cwd).await.ok().flatten() else {
        return unavailable(out, "unsupported", "The repository's remote is not one a pull request can be made on from here.".into());
    };
    out.provider = ctx.kind.to_owned();
    out.repository = Some(ctx.repository.clone());
    if !matches!(ctx.kind, "github" | "gitlab") {
        return unavailable(out, "unsupported", format!("Pull requests cannot be made on {} from here yet.", ctx.host));
    }
    out.supports_draft = true;
    out.closing = thread.focus.iter().map(|issue| closing_reference(issue, &ctx.host, &ctx.repository)).collect();

    // The bases: the one asked for, the thread's own, the default, then the remote's other branches by latest commit.
    let default = default_branch(cwd).await;
    out.on_default_branch = match (&status.branch, &default) {
        (Some(branch), Some(default)) => default == branch,
        (Some(branch), None) => branch == "main" || branch == "master",
        (None, _) => false,
    };
    // The branch the pull request comes from is no base for it; work on the default branch leaves it first.
    let head = status.branch.as_deref().filter(|_| !out.on_default_branch);
    let mut bases: Vec<String> = vec![];
    for wanted in [input.base_branch.clone(), thread.base_branch.clone(), default.clone()].into_iter().flatten() {
        let on_remote = git(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{remote}/{wanted}")]).await.ok;
        if on_remote && Some(wanted.as_str()) != head && !bases.contains(&wanted) {
            bases.push(wanted);
        }
    }
    let others = git(cwd, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:lstrip=3)", &format!("refs/remotes/{remote}")]).await.stdout;
    for branch in others.lines().filter(|b| !b.is_empty() && *b != "HEAD") {
        if bases.len() >= MOST_BASE_BRANCHES {
            break;
        }
        if Some(branch) != head && !bases.iter().any(|b| b == branch) {
            bases.push(branch.to_owned());
        }
    }
    out.base_branch = bases.first().cloned().or(default.clone()).unwrap_or_else(|| "main".into());
    out.base_branches = bases;

    let Some(branch) = status.branch.clone() else {
        return unavailable(out, "detached", "No branch is checked out here: a pull request needs one.".into());
    };
    let base_ref = base_ref_of(cwd, &out.base_branch).await;
    let count = |args: Vec<String>| async move {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(cwd, &args).await.stdout.trim().parse::<u64>().unwrap_or(0)
    };
    out.commits_ahead_of_base = count(vec!["rev-list".into(), "--count".into(), format!("{base_ref}..HEAD")]).await;
    out.unpushed = match status.upstream_ref {
        Some(_) => status.ahead,
        None => count(vec!["rev-list".into(), "--count".into(), "HEAD".into(), "--not".into(), format!("--remotes={remote}")]).await,
    };
    if out.on_default_branch {
        // The work leaves the default branch for one of its own, named for the issue when there is one.
        let naming = writer_settings().await;
        out.new_branch = match issue_branch_of(&naming, &thread) {
            Some(wanted) => Some(crate::vcs_ops::free_branch_name(cwd, &wanted).await),
            None => None,
        };
        if out.dirty_files.is_empty() {
            return unavailable(out, "no-changes", format!("{branch} is the repository's default branch and nothing here is changed: there is nothing to open a pull request for yet."));
        }
    } else {
        match open_change_request(&ctx, cwd, &branch).await {
            Ok(Some(open)) => {
                if let (Some(number), Some(url)) = (open.number, open.url.clone()) {
                    out.existing = Some(PullRequestExisting { number, url, title: open.title.clone().unwrap_or_default(), linked: thread.linked.contains(&number) });
                }
            }
            Ok(None) => {}
            Err(e) if e.is_unusable() => {
                let reason = if e.reason == gh::GhReason::MissingTool { "tool-missing" } else { "unauthenticated" };
                let detail = match (ctx.kind, reason) {
                    ("gitlab", "tool-missing") => crate::glab::REQUIREMENT_MISSING,
                    ("gitlab", _) => crate::glab::REQUIREMENT_UNAUTHENTICATED,
                    (_, "tool-missing") => gh::REQUIREMENT_MISSING,
                    _ => gh::REQUIREMENT_UNAUTHENTICATED,
                };
                return unavailable(out, reason, detail.to_owned());
            }
            // Any other failure of the lookup: making the pull request will say what is wrong.
            Err(e) => tracing::info!(%cwd, "the branch's open pull request was not looked up: {}", e.detail),
        }
        if out.existing.is_none() && out.dirty_files.is_empty() && out.commits_ahead_of_base == 0 {
            let detail = format!("{branch} has no commits ahead of {} and nothing here is changed: there is nothing to open a pull request for yet.", out.base_branch);
            return unavailable(out, "no-changes", detail);
        }
    }

    if input.generate == Some(true) && out.existing.is_none() {
        let hint = thread.issue_hint();
        let head = out.new_branch.as_deref().unwrap_or(&branch);
        match generate_pr_content(cwd, PrSource { base: &out.base_branch, head, uncommitted: true, issue: hint.as_deref() }).await {
            Ok((title, body)) => {
                out.title = Some(title);
                out.body = Some(with_closing(&body, &out.closing));
            }
            Err(e) => {
                // A sheet that can still be filled in: the issue's title, else the latest commit's subject.
                let subject = git(cwd, &["log", "-1", "--format=%s", &format!("{base_ref}..HEAD")]).await.stdout.trim().to_owned();
                out.title = Some(thread.focus.first().map(|issue| issue.title.clone()).filter(|t| !t.is_empty()).or(Some(subject).filter(|s| !s.is_empty())).unwrap_or_default());
                out.body = Some(with_closing("", &out.closing));
                out.generation_error = Some(e.detail);
            }
        }
    }
    Ok(out)
}

/// The branch a thread started on an issue commits to when its work is on the default branch.
fn issue_branch_of(settings: &ServerSettings, thread: &ThreadContext) -> Option<String> {
    let issue = thread.focus.first()?;
    let mode = settings.branch_naming_mode.as_ref().map_or("static", |mode| mode.as_str());
    Some(crate::orchestration::naming::issue_branch(&issue.display_key, &issue.title, mode, settings.branch_name_prefix.as_deref().unwrap_or("domovoi")))
}

/// What the pull request step takes from the action it is a step of.
struct PrStep<'a> {
    /// The branch, when HEAD does not say (just created).
    fallback: Option<&'a str>,
    /// The pull request as the sheet's reader left it.
    sheet: Option<&'a StackedPullRequest>,
    thread: &'a ThreadContext,
}

async fn run_pr_step(cwd: &str, step: PrStep<'_>) -> Result<(GitPrStep, ProviderContext), Failure> {
    let ctx = match detect_provider(cwd).await? {
        Some(ctx) if matches!(ctx.kind, "github" | "gitlab") => ctx,
        other => {
            let kind = other.map(|c| c.kind).unwrap_or("unknown");
            return Err(provider_error(kind, "createChangeRequest", cwd, None, format!("No {kind} source control provider is registered.")));
        }
    };
    let status = status_details(cwd).await?;
    let Some(branch) = status.branch.clone().or(step.fallback.map(str::to_owned)) else {
        return Err(manager_error("runPrStep", cwd, "Cannot create a pull request from detached HEAD."));
    };
    if status.upstream_ref.is_none() {
        return Err(manager_error("runPrStep", cwd, "Current branch has not been pushed. Push before creating a PR."));
    }
    if let Some(existing) = find_open_pr(&ctx, cwd, &branch).await {
        return Ok((existing.step(GitPrStepStatus::OpenedExisting), ctx));
    }
    let closing: Vec<PullRequestClosingRef> = step.thread.focus.iter().map(|issue| closing_reference(issue, &ctx.host, &ctx.repository)).collect();
    let asked_base = step.sheet.and_then(|sheet| sheet.base_branch.clone()).filter(|b| !b.trim().is_empty());
    let base = match asked_base {
        Some(base) => Some(base),
        None => default_branch(cwd).await,
    };
    let content = match step.sheet {
        // As the person left it: nothing is added to what they read.
        Some(sheet) => {
            if sheet.title.trim().is_empty() {
                return Err(manager_error("runPrStep", cwd, "A pull request needs a title."));
            }
            Some((sheet.title.trim().to_owned(), sheet.body.trim().to_owned()))
        }
        None => {
            let hint = step.thread.issue_hint();
            match generate_pr_content(cwd, PrSource { base: base.as_deref().unwrap_or("main"), head: &branch, uncommitted: false, issue: hint.as_deref() }).await {
                Ok((title, body)) => Some((title, with_closing(&body, &closing))),
                Err(e) => {
                    tracing::info!(%cwd, "pull request text not generated, using the commits: {}", e.detail);
                    None
                }
            }
        }
    };
    let draft = step.sheet.is_some_and(|sheet| sheet.draft == Some(true));
    if ctx.kind == "gitlab" {
        let target = base.clone().unwrap_or_else(|| "main".into());
        let subject = git(cwd, &["log", "-1", "--format=%s"]).await.stdout.trim().to_owned();
        let (title, description) = content.unwrap_or_else(|| (if subject.is_empty() { branch.clone() } else { subject }, with_closing("", &closing)));
        let new = super::gitlab::MergeRequestDraft { source_branch: &branch, target_branch: &target, title: &title, description: &description, draft };
        let made = super::gitlab::create_merge_request(Some(cwd), &ctx.host, &ctx.repository, &new)
            .await
            .map_err(|e| provider_error("gitlab", "createChangeRequest", cwd, Some(&branch), e.detail))?;
        return Ok((ListedPullRequest::of_merge_request(made).step(GitPrStepStatus::Created), ctx));
    }
    let mut args = vec!["pr", "create", "--repo", ctx.repository.as_str(), "--head", branch.as_str()];
    match &content {
        Some((title, body)) => args.extend(["--title", title.as_str(), "--body", body.as_str()]),
        None => args.push("--fill"),
    }
    if let Some(b) = &base {
        args.extend(["--base", b.as_str()]);
    }
    if draft {
        args.push("--draft");
    }
    gh::run(Some(cwd), &ctx.host, &args, None)
        .await
        .map_err(|e| provider_error("github", "createChangeRequest", cwd, Some(&branch), e.detail))?;
    let made = match find_open_pr(&ctx, cwd, &branch).await {
        Some(created) => created.step(GitPrStepStatus::Created),
        None => GitPrStep { status: GitPrStepStatus::Created, url: None, number: None, base_branch: None, head_branch: None, title: None },
    };
    Ok((made, ctx))
}

/// `orchestrator` is where the action's thread is read and its pull request
/// linked; without one the action knows no thread.
fn run_stacked_action(orchestrator: Option<Arc<Orchestrator>>, input: StackedInput) -> BoxStream<'static, Result<GitActionProgressEvent, Failure>> {
    async_stream::stream! {
        let cwd = input.cwd.clone();
        let thread = thread_context(orchestrator.as_deref(), input.thread_id.as_deref());
        let action = GitStackedAction::from(input.action.as_str());
        let (action_id, of_cwd, of_action) = (input.action_id.clone(), cwd.clone(), action.clone());
        let phase_started = move |phase: GitActionProgressPhase, label: String| {
            GitActionProgressEvent::PhaseStarted(GitActionPhaseStarted { action_id: action_id.clone(), cwd: of_cwd.clone(), action: of_action.clone(), phase, label })
        };
        let mut phase: Option<GitActionProgressPhase> = None;
        // The steps run inline so each progress event is yielded as it happens.
        let failure: Failure;
        'run: {
            let status = match status_details(&cwd).await {
                Ok(s) => s,
                Err(e) => { failure = e.into(); break 'run; }
            };
            // A commit of chosen files that chooses none commits nothing (and must not take everything).
            let wants_commit = is_commit_action(&input.action) && !input.file_paths.as_ref().is_some_and(Vec::is_empty);
            let wants_push = matches!(action, GitStackedAction::Push | GitStackedAction::CommitPush | GitStackedAction::CommitPushPr)
                || (action == GitStackedAction::CreatePr && (status.upstream_ref.is_none() || status.ahead > 0));
            let wants_pr = matches!(action, GitStackedAction::CreatePr | GitStackedAction::CommitPushPr);
            let feature_branch = input.feature_branch == Some(true);
            if feature_branch && !wants_commit {
                failure = manager_error("runStackedAction", &cwd, "Feature-branch checkout is only supported for commit actions.");
                break 'run;
            }
            let mut phases = vec![];
            if feature_branch { phases.push(GitActionProgressPhase::Branch); }
            if wants_commit { phases.push(GitActionProgressPhase::Commit); }
            if wants_push { phases.push(GitActionProgressPhase::Push); }
            if wants_pr { phases.push(GitActionProgressPhase::Pr); }
            yield Ok(GitActionProgressEvent::ActionStarted(GitActionStarted { action_id: input.action_id.clone(), cwd: cwd.clone(), action: action.clone(), phases }));
            if !feature_branch && (wants_push || wants_pr) && status.branch.is_none() {
                let what = if wants_push { "Cannot push from detached HEAD." } else { "Cannot create a pull request from detached HEAD." };
                failure = manager_error("runStackedAction", &cwd, what);
                break 'run;
            }
            let provider_kind = detect_provider(&cwd).await.ok().flatten().map(|c| c.kind);
            let (short_label, _, _) = terms(provider_kind);
            let paths = input.file_paths.as_deref();
            let mut custom = input.commit_message.as_deref().and_then(parse_commit_message);

            let mut branch_step = GitBranchStep { status: GitBranchStepStatus::SkippedNotRequested, name: None };
            let mut current_branch = status.branch.clone();
            if feature_branch {
                phase = Some(GitActionProgressPhase::Branch);
                yield Ok(phase_started(GitActionProgressPhase::Branch, "Preparing feature branch...".into()));
                match stage(&cwd, paths).await {
                    Ok(true) => {}
                    Ok(false) => { failure = manager_error("runFeatureBranchStep", &cwd, "Cannot create a feature branch because there are no changes to commit."); break 'run; }
                    Err(e) => { failure = e.into(); break 'run; }
                }
                // A thread started on an issue commits to a branch named for it; nothing is generated for the name.
                let naming = writer_settings().await;
                let name = match issue_branch_of(&naming, &thread) {
                    Some(wanted) => crate::vcs_ops::free_branch_name(&cwd, &wanted).await,
                    None => {
                        let fragment = match custom.clone() {
                            Some((subject, _)) => subject,
                            None => match generate_commit_message(&cwd, current_branch.as_deref(), paths, true).await {
                                Ok((subject, body, branch)) => {
                                    custom = Some((subject.clone(), body));
                                    branch.filter(|b| !b.trim().is_empty()).unwrap_or(subject)
                                }
                                Err(e) => { failure = e.into(); break 'run; }
                            },
                        };
                        let existing: Vec<String> = git(&cwd, &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"]).await.stdout.lines().map(str::to_owned).collect();
                        resolve_feature_branch(&existing, &sanitize_feature_branch(&fragment))
                    }
                };
                if let Err(e) = git_ok("GitVcsDriver.switchRef", &cwd, &["switch", "-c", &name]).await { failure = e.into(); break 'run; }
                branch_step = GitBranchStep { status: GitBranchStepStatus::Created, name: Some(name.clone()) };
                current_branch = Some(name);
            }

            let mut commit_step = GitCommitStep { status: GitCommitStepStatus::SkippedNotRequested, commit_sha: None, subject: None };
            if wants_commit {
                phase = Some(GitActionProgressPhase::Commit);
                let has_changes = match stage(&cwd, paths).await {
                    Ok(c) => c,
                    Err(e) => { failure = e.into(); break 'run; }
                };
                if !has_changes {
                    commit_step.status = GitCommitStepStatus::SkippedNoChanges;
                } else {
                    let (subject, body) = match custom.clone() {
                        Some(m) => m,
                        None => {
                            yield Ok(phase_started(GitActionProgressPhase::Commit, "Generating commit message...".into()));
                            match generate_commit_message(&cwd, current_branch.as_deref(), paths, false).await {
                                Ok((s, b, _)) => (s, b),
                                Err(e) => { failure = e.into(); break 'run; }
                            }
                        }
                    };
                    yield Ok(phase_started(GitActionProgressPhase::Commit, "Committing...".into()));
                    let mut args: Vec<&str> = vec!["commit", "-m", &subject];
                    if !body.is_empty() { args.extend(["-m", &body]); }
                    if let Some(p) = paths { args.push("--"); args.extend(p.iter().map(String::as_str)); }
                    if let Err(e) = git_ok("GitVcsDriver.commit", &cwd, &args).await { failure = e.into(); break 'run; }
                    let sha = head_sha(&cwd).await;
                    commit_step = GitCommitStep { status: GitCommitStepStatus::Created, commit_sha: sha, subject: Some(subject) };
                }
            }

            let mut push_step = GitPushStep { status: GitPushStepStatus::SkippedNotRequested, branch: None, upstream_branch: None, set_upstream: None };
            if wants_push {
                yield Ok(phase_started(GitActionProgressPhase::Push, "Pushing...".into()));
                phase = Some(GitActionProgressPhase::Push);
                match push_current_branch(&cwd, current_branch.as_deref()).await {
                    Ok(p) => push_step = p,
                    Err(e) => { failure = e.into(); break 'run; }
                }
            }

            let mut pr_step = GitPrStep { status: GitPrStepStatus::SkippedNotRequested, url: None, number: None, base_branch: None, head_branch: None, title: None };
            if wants_pr {
                yield Ok(phase_started(GitActionProgressPhase::Pr, format!("Preparing {short_label}...")));
                phase = Some(GitActionProgressPhase::Pr);
                match run_pr_step(&cwd, PrStep { fallback: current_branch.as_deref(), sheet: input.pull_request.as_ref(), thread: &thread }).await {
                    Ok((made, ctx)) => {
                        if let (Some(orchestrator), Some(thread_id)) = (orchestrator.as_deref(), input.thread_id.as_deref()) {
                            link_to_thread(orchestrator, thread_id, &ctx, &made).await;
                        }
                        pr_step = made;
                    }
                    Err(e) => { failure = e; break 'run; }
                }
            }

            let on_default = match (status_details(&cwd).await.ok().and_then(|s| s.branch), default_branch(&cwd).await) {
                (Some(b), Some(d)) => b == d,
                (Some(b), None) => b == "main" || b == "master",
                _ => false,
            };
            let toast = completion_toast(&action, &commit_step, &push_step, &pr_step, short_label, on_default);
            let result = GitRunStackedActionResult { action: action.clone(), branch: branch_step, commit: commit_step, push: push_step, pr: pr_step, toast };
            yield Ok(GitActionProgressEvent::ActionFinished(Box::new(GitActionFinished { action_id: input.action_id.clone(), cwd, action, result })));
            return;
        }
        let message = failure.detail().unwrap_or("The action failed.").to_owned();
        yield Ok(GitActionProgressEvent::ActionFailed(GitActionFailed { action_id: input.action_id.clone(), cwd, action, phase, message }));
        yield Err(failure);
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("git.resolvePullRequest", |_, input: ResolveInput| resolve_pull_request(input))
        .unary("git.preparePullRequestThread", |ctx: Arc<ConnCtx>, input: PrepareInput| prepare_pull_request_thread(ctx.state.clone(), input))
        .unary("git.preparePullRequest", |ctx: Arc<ConnCtx>, input: PullRequestPrepareInput| async move { prepare_pull_request(&ctx.state.orchestrator, input).await })
        .stream("git.runStackedAction", |ctx: Arc<ConnCtx>, input: StackedInput| run_stacked_action(Some(ctx.state.orchestrator.clone()), input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writing_rules_are_read_from_the_settings_record() {
        let of = |style: Value| -> ServerSettings { serde_json::from_value(json!({ "sourceControlWritingStyle": style })).unwrap() };
        let templates = |settings: &ServerSettings| writing_style(settings).and_then(|style| style.follow_change_request_templates) != Some(false);
        let custom = of(json!({ "mode": "custom", "customInstructions": "Use emoji", "followChangeRequestTemplates": false }));
        assert_eq!((writing_instructions(&custom).as_deref(), templates(&custom)), (Some("Use emoji"), false));
        // Instructions count only in the custom mode, and only when they say something.
        assert_eq!(writing_instructions(&of(json!({ "mode": "repo_conventions", "customInstructions": "Use emoji" }))), None);
        assert_eq!(writing_instructions(&of(json!({ "mode": "custom", "customInstructions": "  " }))), None);
        assert_eq!(writing_instructions(&of(json!({ "mode": "custom" }))), None);
        // The instructions alone, as older servers stored them, and no setting at all: the defaults.
        for settings in [of(json!("Use emoji")), ServerSettings::default()] {
            assert_eq!((writing_instructions(&settings), templates(&settings)), (None, true));
        }
    }

    #[test]
    fn the_line_that_closes_an_issue_says_where_the_issue_is() {
        use domovoi_model::typed::{ThreadIssueKey, ThreadIssueLinkSource, ThreadIssueRole};
        let issue = |tracker: &str, host: &str, scope: &str, key: &str, display: &str| ThreadIssueLink {
            key: ThreadIssueKey { tracker: tracker.into(), host: host.into(), scope: scope.into(), key: key.into() },
            project_id: "p".into(),
            url: format!("https://{host}/{scope}/issues/{key}"),
            source: ThreadIssueLinkSource::Manual,
            linked_at: "t".into(),
            display_key: display.into(),
            title: "A bug".into(),
            role: Some(ThreadIssueRole::Focus),
        };
        let line = |issue: &ThreadIssueLink| closing_reference(issue, "github.com", "acme/app").reference;
        assert_eq!(line(&issue("github", "GitHub.com", "Acme/App", "12", "#12")), "Closes #12");
        assert_eq!(line(&issue("github", "github.com", "acme/upstream", "12", "#12")), "Closes acme/upstream#12", "a fork's issue lives upstream");
        assert_eq!(line(&issue("linear", "linear.app", "ENG", "ENG-4", "ENG-4")), "Fixes ENG-4");
        assert_eq!(line(&issue("gitlab", "gitlab.com", "g/p", "3", "#3")), "Closes https://gitlab.com/g/p/issues/3", "another host is cited");
        assert_eq!(closing_reference(&issue("gitlab", "gitlab.com", "g/p", "3", "#3"), "gitlab.com", "g/p").reference, "Closes #3");
    }

    #[test]
    fn closing_lines_end_a_body_once() {
        let closing = |lines: &[&str]| -> Vec<PullRequestClosingRef> {
            lines.iter().map(|line| PullRequestClosingRef { display_key: "#1".into(), reference: (*line).to_owned(), url: "u".into() }).collect()
        };
        assert_eq!(with_closing("## Summary\n- x\n", &closing(&["Closes #12"])), "## Summary\n- x\n\nCloses #12");
        assert_eq!(with_closing("", &closing(&["Closes #12", "Fixes ENG-4"])), "Closes #12\nFixes ENG-4");
        assert_eq!(with_closing("Done.\n\ncloses #12", &closing(&["Closes #12"])), "Done.\n\ncloses #12", "the model wrote it anyway");
        assert_eq!(with_closing("  Done.  ", &[]), "Done.");
    }

    #[test]
    fn a_gitlab_named_host_is_a_gitlab() {
        assert_eq!(provider_kind("gitlab.example.com"), "gitlab");
        assert_eq!(provider_kind("github.com"), "github");
        assert_eq!(provider_kind("git.example.com"), "unknown");
    }

    #[test]
    fn sanitizes_branch_names() {
        assert_eq!(sanitize_branch_fragment("  Fix the 'Login' bug!  "), "fix-the-login-bug");
        assert_eq!(sanitize_branch_fragment("..."), "update");
        assert_eq!(sanitize_feature_branch("Add tests"), "feature/add-tests");
        assert_eq!(resolve_feature_branch(&["feature/add-tests".into()], "feature/add-tests"), "feature/add-tests-2");
        assert_eq!(normalize_reference(" #42 "), "42");
        assert_eq!(normalize_reference("https://github.com/o/r/pull/1"), "https://github.com/o/r/pull/1");
    }

    #[test]
    fn names_cross_repository_worktree_branches() {
        assert_eq!(worktree_local_branch(5, "main", false, &[]), "main");
        assert_eq!(worktree_local_branch(5, "fix/x", true, &[]), "domovoi/pr-5/fix/x");
        assert_eq!(worktree_local_branch(5, "x", true, &["domovoi".into()]), "domovoi-pr-5-x");
    }

    #[test]
    fn parses_commit_messages() {
        assert_eq!(parse_commit_message("Subject\r\n\r\nBody line"), Some(("Subject".into(), "Body line".into())));
        assert_eq!(parse_commit_message("  \n"), None);
    }

    #[test]
    fn failures_are_written_as_tagged_errors() {
        assert_eq!(
            json!(manager_error("runStackedAction", "/repo", "Cannot push from detached HEAD.")),
            json!({ "_tag": "GitManagerError", "operation": "runStackedAction", "cwd": "/repo", "detail": "Cannot push from detached HEAD." })
        );
        assert_eq!(
            json!(provider_error("unknown", "detectProvider", "/repo", None, "Failed to detect source control provider.")),
            json!({ "_tag": "SourceControlProviderError", "provider": "unknown", "operation": "detectProvider", "cwd": "/repo", "detail": "Failed to detect source control provider." })
        );
        assert_eq!(
            json!(provider_error("github", "getChangeRequest", "/repo", Some("7"), "not found")),
            json!({ "_tag": "SourceControlProviderError", "provider": "github", "operation": "getChangeRequest", "cwd": "/repo", "detail": "not found", "reference": "7" })
        );
        let fetch = command_error("GitManager.preparePullRequestThread.fetch", "/repo", &["fetch", "--", "origin", "refs/pull/7/head:refs/heads/x"], "fatal: couldn't find remote ref", Some(128));
        let fetch_json = json!({ "_tag": "GitCommandError", "operation": "GitManager.preparePullRequestThread.fetch", "command": "git fetch", "cwd": "/repo",
            "detail": "fatal: couldn't find remote ref", "argumentCount": 4, "exitCode": 128 });
        assert_eq!(json!(Failure::from(fetch.clone())), fetch_json);
        let materialization = |head_repository: Option<&str>| Failure::Materialization {
            cwd: "/repo".into(), pull_request_number: 7, head_repository: head_repository.map(str::to_owned), head_branch: "fix".into(), local_branch: "domovoi/pr-7/fix".into(), cause: Box::new(fetch.clone()),
        };
        assert_eq!(
            json!(materialization(None)),
            json!({ "_tag": "GitPullRequestMaterializationError", "cwd": "/repo", "pullRequestNumber": 7, "headRepository": null, "headBranch": "fix",
                "localBranch": "domovoi/pr-7/fix", "cause": fetch_json })
        );
        assert_eq!(json!(materialization(Some("forker/widgets")))["headRepository"], "forker/widgets");
        assert_eq!(
            json!(command_error("GitVcsDriver.pushCurrentBranch", "/repo", &["push"], "Cannot push from detached HEAD.", None)),
            json!({ "_tag": "GitCommandError", "operation": "GitVcsDriver.pushCurrentBranch", "command": "git push", "cwd": "/repo", "detail": "Cannot push from detached HEAD.", "argumentCount": 1 })
        );
        let unwritten = Failure::from(crate::text_gen::error("generateCommitMessage", "The model returned no subject."));
        assert_eq!(json!(unwritten), json!({ "_tag": "TextGenerationError", "operation": "generateCommitMessage", "detail": "The model returned no subject." }));
        assert_eq!((unwritten.detail(), materialization(None).detail()), (Some("The model returned no subject."), None));
    }

    #[test]
    fn a_pull_request_is_read_from_what_gh_says() {
        let pr = ViewedPullRequest::of(&json!({ "number": 8, "title": "Eight", "url": "https://github.com/o/r/pull/8", "baseRefName": "main", "headRefName": "Fix/Thing",
            "state": "OPEN", "mergedAt": "2024-02-03T04:05:06Z", "headRefOid": "abc", "isCrossRepository": true, "headRepositoryOwner": { "login": "forker" }, "headRepository": { "name": "r" } }));
        assert_eq!(
            json!(pr.resolved),
            json!({ "number": 8, "title": "Eight", "url": "https://github.com/o/r/pull/8", "baseBranch": "main", "headBranch": "Fix/Thing", "state": "merged" })
        );
        assert_eq!((pr.number, pr.head_branch.as_str(), pr.head_sha.as_deref(), pr.cross_repository, pr.head_repository.as_deref()), (8, "Fix/Thing", Some("abc"), true, Some("forker/r")));
        let bare = ViewedPullRequest::of(&json!({ "number": 9 }));
        assert_eq!(json!(bare.resolved), json!({ "number": 9, "title": "", "url": "", "baseBranch": "", "headBranch": "", "state": "open" }));
        assert_eq!((bare.head_sha, bare.cross_repository, bare.head_repository), (None, false, None));
    }

    #[test]
    fn builds_completion_toasts() {
        let commit = GitCommitStep { status: GitCommitStepStatus::Created, commit_sha: Some("abcdef1234".into()), subject: Some("Add x".into()) };
        let not_pushed = GitPushStep { status: GitPushStepStatus::SkippedNotRequested, branch: None, upstream_branch: None, set_upstream: None };
        let no_pr = GitPrStep { status: GitPrStepStatus::SkippedNotRequested, url: None, number: None, base_branch: None, head_branch: None, title: None };
        let t = completion_toast(&GitStackedAction::Commit, &commit, &not_pushed, &no_pr, "PR", false);
        assert_eq!(json!(t), json!({ "title": "Committed abcdef1", "description": "Add x", "cta": { "kind": "run_action", "label": "Push", "action": { "kind": "push" } } }));
        let pushed = GitPushStep { status: GitPushStepStatus::Pushed, branch: Some("feat".into()), upstream_branch: Some("origin/feat".into()), set_upstream: None };
        let t = completion_toast(&GitStackedAction::CommitPush, &commit, &pushed, &no_pr, "PR", false);
        assert_eq!(json!(t), json!({ "title": "Pushed abcdef1 to origin/feat", "description": "Add x", "cta": { "kind": "run_action", "label": "Create PR", "action": { "kind": "create_pr" } } }));
        let t = completion_toast(&GitStackedAction::CommitPush, &commit, &pushed, &no_pr, "PR", true);
        assert_eq!(json!(t.cta), json!({ "kind": "none" }), "nothing to open from the default branch");
        let pr = GitPrStep { status: GitPrStepStatus::Created, url: Some("https://github.com/o/r/pull/3".into()), number: Some(3), base_branch: None, head_branch: None, title: Some("Three".into()) };
        let t = completion_toast(&GitStackedAction::CommitPushPr, &commit, &pushed, &pr, "PR", false);
        assert_eq!(json!(t), json!({ "title": "Created PR #3", "description": "Three", "cta": { "kind": "open_pr", "label": "View PR", "url": "https://github.com/o/r/pull/3" } }));
        let nothing = GitCommitStep { status: GitCommitStepStatus::SkippedNoChanges, commit_sha: None, subject: None };
        assert_eq!(json!(completion_toast(&GitStackedAction::Commit, &nothing, &not_pushed, &no_pr, "PR", false)), json!({ "title": "Done", "cta": { "kind": "none" } }));
    }

    fn temp_repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("domovoi-gw-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@t"]] {
            assert!(std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        }
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        dir
    }

    /// The events of a git action as JSON, and the failure it ended with.
    async fn collect(input: StackedInput) -> (Vec<Value>, Option<Value>) {
        let items: Vec<Result<GitActionProgressEvent, Failure>> = run_stacked_action(None, input).collect().await;
        let mut events = vec![];
        let mut failure = None;
        for item in items {
            match item {
                Ok(event) => events.push(json!(event)),
                Err(e) => failure = Some(json!(e)),
            }
        }
        (events, failure)
    }

    fn stacked(cwd: &std::path::Path, action: &str, message: Option<&str>) -> StackedInput {
        StackedInput {
            action_id: "a".into(), cwd: cwd.to_string_lossy().into_owned(), action: action.into(),
            commit_message: message.map(str::to_owned), feature_branch: None, file_paths: None, thread_id: None, pull_request: None,
        }
    }

    #[tokio::test]
    async fn commits_then_fails_to_push_without_a_remote() {
        let dir = temp_repo("push");
        let (events, failure) = collect(stacked(&dir, "commit_push", Some("Add a\n\nBody"))).await;
        let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["action_started", "phase_started", "phase_started", "action_failed"]);
        let cwd = dir.to_string_lossy();
        assert_eq!(events[0], json!({ "actionId": "a", "cwd": cwd, "action": "commit_push", "kind": "action_started", "phases": ["commit", "push"] }));
        assert_eq!(events[1], json!({ "actionId": "a", "cwd": cwd, "action": "commit_push", "kind": "phase_started", "phase": "commit", "label": "Committing..." }));
        assert_eq!(
            events[3],
            json!({ "actionId": "a", "cwd": cwd, "action": "commit_push", "kind": "action_failed", "phase": "push",
                "message": "Cannot push because no git remote is configured for this repository." })
        );
        let err = failure.unwrap();
        assert_eq!(err["_tag"], "GitCommandError");
        assert_eq!(err["detail"], "Cannot push because no git remote is configured for this repository.");
        let log = std::process::Command::new("git").args(["log", "--format=%s%n%b", "-1"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "Add a\nBody");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn commit_reports_created_and_no_changes() {
        let dir = temp_repo("commit");
        let (events, failure) = collect(stacked(&dir, "commit", Some("First"))).await;
        assert!(failure.is_none());
        let finished = events.last().unwrap();
        assert_eq!(finished["kind"], "action_finished");
        let result = &finished["result"];
        assert_eq!(result["commit"]["status"], "created");
        assert_eq!(result["commit"]["subject"], "First");
        assert_eq!(result["toast"]["cta"]["kind"], "run_action");
        assert_eq!((&result["action"], &result["branch"], &result["push"], &result["pr"]),
            (&json!("commit"), &json!({ "status": "skipped_not_requested" }), &json!({ "status": "skipped_not_requested" }), &json!({ "status": "skipped_not_requested" })));
        let (again, _) = collect(stacked(&dir, "commit", Some("Second"))).await;
        assert_eq!(again.last().unwrap()["result"]["commit"], json!({ "status": "skipped_no_changes" }));
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        let (_, unwritten) = collect(stacked(&dir, "commit", None)).await;
        assert_eq!(unwritten.unwrap()["_tag"], "TextGenerationError");
        let (feature, _) = collect(StackedInput { feature_branch: Some(true), ..stacked(&dir, "commit", Some("Two")) }).await;
        let result = &feature.last().unwrap()["result"];
        assert_eq!(result["branch"], json!({ "status": "created", "name": "feature/two" }));
        let (refused, failure) = collect(StackedInput { feature_branch: Some(true), ..stacked(&dir, "push", None) }).await;
        assert_eq!(refused[0]["kind"], "action_failed");
        assert_eq!(refused[0]["phase"], json!(null));
        assert_eq!(failure.unwrap()["_tag"], "GitManagerError");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
