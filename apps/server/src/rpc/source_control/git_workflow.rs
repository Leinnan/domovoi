//! `git.resolvePullRequest`, `git.preparePullRequestThread` and
//! `git.runStackedAction`, after t3code's `GitManager`.
//!
//! The source control provider of a checkout comes from its primary remote
//! (`origin`, else the first); only GitHub is served (through `gh`), and other
//! hosts fail as t3code's unregistered providers do.
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
};

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

/// t3code's `normalizePullRequestReference`.
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

/// t3code's `resolvePullRequestWorktreeLocalBranchName`.
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
    let preferred = format!("t3code/pr-{number}/{suffix}");
    if creatable(&preferred) {
        return preferred;
    }
    let flat = format!("t3code-pr-{number}-{}", suffix.replace('/', "-"));
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
}

fn is_commit_action(action: &str) -> bool {
    matches!(action, "commit" | "commit_push" | "commit_push_pr")
}

/// t3code's `parseCustomCommitMessage`.
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

/// A pull request title and body from the branch's commits and diff.
async fn generate_pr_content(cwd: &str, base: &str, head: &str) -> Result<(String, String), TextGenerationError> {
    let remote_base = format!("origin/{base}");
    let base_ref = if git(cwd, &["rev-parse", "--verify", "--quiet", &remote_base]).await.ok { remote_base } else { base.to_owned() };
    let range = format!("{base_ref}...HEAD");
    let commits = git(cwd, &["log", "--format=- %s%n%b", &format!("{base_ref}..HEAD")]).await.stdout;
    let stat = git(cwd, &["diff", "--stat", &range]).await.stdout;
    let patch = git(cwd, &["diff", "--no-color", &range]).await.stdout;
    let settings = writer_settings().await;
    let template = (writing_style(&settings).and_then(|style| style.follow_change_request_templates) != Some(false)).then(|| {
        [".github/pull_request_template.md", ".github/PULL_REQUEST_TEMPLATE.md", "docs/pull_request_template.md", "PULL_REQUEST_TEMPLATE.md", "pull_request_template.md"]
            .iter().find_map(|p| std::fs::read_to_string(Path::new(cwd).join(p)).ok())
    }).flatten();
    let prompt = crate::text_gen::pr_content_prompt(base, head, &commits, &stat, &patch, template.as_deref(), writing_instructions(&settings).as_deref());
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

/// t3code's `resolveAutoFeatureBranchName`.
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

async fn find_open_pr(ctx: &ProviderContext, cwd: &str, branch: &str) -> Option<ListedPullRequest> {
    // What `gh pr list` printed, read into a record here.
    let raw = gh::run_json::<Value>(
        Some(cwd), &ctx.host,
        &["pr", "list", "--repo", &ctx.repository, "--head", branch, "--state", "open", "--limit", "1", "--json", "number,url,title,baseRefName,headRefName"],
    )
    .await
    .ok()?;
    let first = raw.as_array()?.first()?;
    let text = |key: &str| first[key].as_str().map(str::to_owned);
    Some(ListedPullRequest { number: first["number"].as_u64(), url: text("url"), title: text("title"), base_branch: text("baseRefName"), head_branch: text("headRefName") })
}

async fn run_pr_step(cwd: &str, fallback: Option<&str>) -> Result<GitPrStep, Failure> {
    let ctx = match detect_provider(cwd).await? {
        Some(ctx) if ctx.kind == "github" => ctx,
        other => {
            let kind = other.map(|c| c.kind).unwrap_or("unknown");
            return Err(provider_error(kind, "createChangeRequest", cwd, None, format!("No {kind} source control provider is registered.")));
        }
    };
    let status = status_details(cwd).await?;
    let Some(branch) = status.branch.clone().or(fallback.map(str::to_owned)) else {
        return Err(manager_error("runPrStep", cwd, "Cannot create a pull request from detached HEAD."));
    };
    if status.upstream_ref.is_none() {
        return Err(manager_error("runPrStep", cwd, "Current branch has not been pushed. Push before creating a PR."));
    }
    if let Some(existing) = find_open_pr(&ctx, cwd, &branch).await {
        return Ok(existing.step(GitPrStepStatus::OpenedExisting));
    }
    let base = default_branch(cwd).await;
    let content = match generate_pr_content(cwd, base.as_deref().unwrap_or("main"), &branch).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::info!(%cwd, "pull request text not generated, using the commits: {}", e.detail);
            None
        }
    };
    let mut args = vec!["pr", "create", "--repo", ctx.repository.as_str(), "--head", branch.as_str()];
    match &content {
        Some((title, body)) => args.extend(["--title", title.as_str(), "--body", body.as_str()]),
        None => args.push("--fill"),
    }
    if let Some(b) = &base {
        args.extend(["--base", b.as_str()]);
    }
    gh::run(Some(cwd), &ctx.host, &args, None)
        .await
        .map_err(|e| provider_error("github", "createChangeRequest", cwd, Some(&branch), e.detail))?;
    Ok(match find_open_pr(&ctx, cwd, &branch).await {
        Some(created) => created.step(GitPrStepStatus::Created),
        None => GitPrStep { status: GitPrStepStatus::Created, url: None, number: None, base_branch: None, head_branch: None, title: None },
    })
}

fn run_stacked_action(input: StackedInput) -> BoxStream<'static, Result<GitActionProgressEvent, Failure>> {
    async_stream::stream! {
        let cwd = input.cwd.clone();
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
            let wants_commit = is_commit_action(&input.action);
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
                let name = resolve_feature_branch(&existing, &sanitize_feature_branch(&fragment));
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
                match run_pr_step(&cwd, current_branch.as_deref()).await {
                    Ok(p) => pr_step = p,
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
        .stream("git.runStackedAction", |_, input: StackedInput| run_stacked_action(input))
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
    fn sanitizes_branch_names_like_t3code() {
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
        assert_eq!(worktree_local_branch(5, "fix/x", true, &[]), "t3code/pr-5/fix/x");
        assert_eq!(worktree_local_branch(5, "x", true, &["t3code".into()]), "t3code-pr-5-x");
    }

    #[test]
    fn parses_commit_messages() {
        assert_eq!(parse_commit_message("Subject\r\n\r\nBody line"), Some(("Subject".into(), "Body line".into())));
        assert_eq!(parse_commit_message("  \n"), None);
    }

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
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
            cwd: "/repo".into(), pull_request_number: 7, head_repository: head_repository.map(str::to_owned), head_branch: "fix".into(), local_branch: "t3code/pr-7/fix".into(), cause: Box::new(fetch.clone()),
        };
        assert_eq!(
            json!(materialization(None)),
            json!({ "_tag": "GitPullRequestMaterializationError", "cwd": "/repo", "pullRequestNumber": 7, "headRepository": null, "headBranch": "fix",
                "localBranch": "t3code/pr-7/fix", "cause": fetch_json })
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
        let items: Vec<Result<GitActionProgressEvent, Failure>> = run_stacked_action(input).collect().await;
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
            commit_message: message.map(str::to_owned), feature_branch: None, file_paths: None,
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
