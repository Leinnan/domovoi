//! `sourceControl.*`, `projectClone.*` and `subscribeProjectClones`, after
//! t3code's `SourceControlRepositoryService`, `SourceControlSignIn` and
//! `ProjectCloneTracker`.
//!
//! - Repository lookups, publishing and starring go through the GitHub CLI
//!   (`crate::gh`); other providers answer as t3code does for a provider that
//!   is not registered.
//! - Sign-in is t3code's GitHub App device flow, which needs t3code's own App
//!   registration, so every provider answers `unsupported` (as t3code does for
//!   providers without one) and app access is `signed-out`.
//! - Clones run `git clone --progress` and report its progress like t3code;
//!   tracked clones live in memory, so a restart forgets them (as in t3code).

#![expect(clippy::disallowed_types, reason = "what `gh repo view` printed")]

use super::super::Never;
use crate::gh;
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use domovoi_model::typed::{ProjectId, Timestamp};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
const STALL_AFTER: Duration = Duration::from_secs(20);
const DONE_RETENTION: Duration = Duration::from_secs(30);
const DETAIL_MAX: usize = 200;
const ERROR_MAX: usize = 1000;

/// What was being done to a repository when it failed (`SourceControlRepositoryError.operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Operation {
    LookupRepository,
    CloneRepository,
    PublishRepository,
    /// Removing what a failed or cancelled clone left behind.
    DiscardClone,
}

/// Why a sign-in did not go on (`SourceControlSignInFailure`): Domovoi has no
/// sign-in of its own, so this is the only one it gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SignInReason {
    Unsupported,
}

/// A failed call of this module, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
enum Failure {
    /// `provider` is a source control provider kind, or `unknown`.
    #[error("{detail}")]
    #[serde(rename = "SourceControlRepositoryError")]
    Repository { provider: String, operation: Operation, detail: String },

    #[error("{detail}")]
    #[serde(rename = "SourceControlSignInError")]
    SignIn { provider: String, host: String, reason: SignInReason, detail: String },

    /// The project of a clone could not be created.
    #[error("{message}")]
    #[serde(rename = "OrchestrationDispatchCommandError")]
    Dispatch { message: String },
}

impl Failure {
    fn repository(provider: &str, operation: Operation, detail: impl Into<String>) -> Self {
        Failure::Repository { provider: provider.to_owned(), operation, detail: detail.into() }
    }

    /// The same failure as one of `operation`: a lookup that fails inside a
    /// clone is the clone's failure.
    fn during(self, operation: Operation) -> Self {
        match self {
            Failure::Repository { provider, detail, .. } => Failure::Repository { provider, operation, detail },
            other => other,
        }
    }

    fn sign_in(provider: &str, host: &str) -> Self {
        Failure::SignIn {
            provider: provider.to_owned(),
            host: host.to_owned(),
            reason: SignInReason::Unsupported,
            detail: format!("This server cannot sign in to {provider}. Use a token or the host's CLI."),
        }
    }
}

const GENERIC_DETAIL: &str = "The source control operation could not be completed.";

/// Credentials are stripped from URLs shown to clients; git still receives them.
pub(crate) fn redact_remote_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let path = path.split_once('?').map_or(path, |(p, _)| p);
    if host == authority && !rest.contains('?') {
        return url.to_owned();
    }
    let path = if path.is_empty() { "/" } else { path };
    format!("{scheme}://{host}{path}")
}

fn clamp(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}…", text.chars().take(max - 1).collect::<String>()) }
}

/// `~` expansion plus a lexical `path.resolve`.
fn resolve_path(raw: &str) -> PathBuf {
    let expanded = if raw == "~" {
        crate::util::home_dir()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        crate::util::home_dir().join(rest)
    } else {
        PathBuf::from(raw)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(expanded)
    };
    let mut out = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Repository lookup / clone / publish
// ---------------------------------------------------------------------------

/// A repository on its host (`SourceControlRepositoryInfo`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryInfo {
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    provider: String,
    name_with_owner: String,
    url: String,
    ssh_url: String,
}

/// `gh repo view`: the repository as GitHub names it. A field `gh` leaves out,
/// or prints as anything but a string, is empty.
async fn view_repository(provider: &str, repository: &str, cwd: Option<&str>) -> Result<RepositoryInfo, gh::GhError> {
    // What `gh repo view` printed.
    let raw = gh::run_json::<serde_json::Value>(cwd, "github.com", &["repo", "view", repository, "--json", "nameWithOwner,url,sshUrl"]).await?;
    let text = |key: &str| raw[key].as_str().unwrap_or("").to_owned();
    Ok(RepositoryInfo { provider: provider.to_owned(), name_with_owner: text("nameWithOwner"), url: text("url"), ssh_url: text("sshUrl") })
}

async fn lookup_repository(provider: &str, repository: &str, cwd: Option<&str>) -> Result<RepositoryInfo, Failure> {
    if provider == "unknown" {
        return Err(Failure::repository(provider, Operation::LookupRepository, "Choose a source control provider before continuing."));
    }
    if provider != "github" {
        return Err(Failure::repository(provider, Operation::LookupRepository, GENERIC_DETAIL));
    }
    view_repository(provider, repository.trim(), cwd).await.map_err(|e| Failure::repository(provider, Operation::LookupRepository, e.detail))
}

/// How a client asks for a repository to be reached (`SourceControlCloneProtocol`).
/// Any text but `ssh` and `https` is `auto`: ssh first, https when ssh is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloneProtocol {
    Auto,
    Ssh,
    Https,
}

impl<'de> Deserialize<'de> for CloneProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(sent)?.as_str() {
            "ssh" => CloneProtocol::Ssh,
            "https" => CloneProtocol::Https,
            _ => CloneProtocol::Auto,
        })
    }
}

fn select_remote_url(repository: &RepositoryInfo, protocol: Option<CloneProtocol>) -> String {
    if protocol == Some(CloneProtocol::Https) { repository.url.clone() } else { repository.ssh_url.clone() }
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct CloneInput {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    remote_url: Option<String>,
    destination_path: String,
    #[serde(default)]
    protocol: Option<CloneProtocol>,
}

struct PreparedClone {
    destination: PathBuf,
    remote_url: String,
    clone_url: String,
    /// The repository as its host names it, when the clone was asked for by name.
    repository: Option<RepositoryInfo>,
}

fn prepare_destination(raw: &str) -> Result<PathBuf, Failure> {
    let fail = |detail: &str| Failure::repository("unknown", Operation::CloneRepository, detail);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(fail("Choose a destination path before cloning."));
    }
    let dest = resolve_path(trimmed);
    if dest.exists() {
        let entries = std::fs::read_dir(&dest).map_err(|_| fail("Destination path already exists and is not a directory."))?;
        if entries.count() > 0 {
            return Err(fail("Destination path already exists and is not empty."));
        }
    } else if let Some(parent) = dest.parent().filter(|p| !p.exists()) {
        std::fs::create_dir_all(parent).map_err(|e| fail(&e.to_string()))?;
    }
    Ok(dest)
}

async fn prepare_clone(input: &CloneInput) -> Result<PreparedClone, Failure> {
    let destination = prepare_destination(&input.destination_path)?;
    let provider = input.provider.clone().unwrap_or_else(|| "unknown".into());
    let mut remote_url = input.remote_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let mut repository = None;
    if let (Some(p), Some(r)) = (&input.provider, &input.repository) {
        let parent = destination.parent().map(|p| p.to_string_lossy().into_owned());
        let found = lookup_repository(p, r, parent.as_deref()).await.map_err(|e| e.during(Operation::CloneRepository))?;
        remote_url = Some(select_remote_url(&found, input.protocol));
        repository = Some(found);
    }
    let Some(clone_url) = remote_url.filter(|u| !u.is_empty()) else {
        return Err(Failure::repository(&provider, Operation::CloneRepository, "Enter a repository path or clone URL before cloning."));
    };
    Ok(PreparedClone { destination, remote_url: redact_remote_url(&clone_url), clone_url, repository })
}

/// What `git clone` is doing (`ProjectCloneStage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CloneStage {
    /// Before git reports anything.
    Connecting,
    Counting,
    Receiving,
    Resolving,
    Checkout,
}

/// One line of `git clone --progress` stderr (t3code's `parseGitCloneProgressLine`).
pub fn parse_clone_progress(line: &str) -> Option<(CloneStage, Option<u64>, Option<String>)> {
    let t = line.trim();
    const STAGES: [(&str, CloneStage); 7] = [
        ("remote: Enumerating objects", CloneStage::Counting),
        ("remote: Counting objects", CloneStage::Counting),
        ("remote: Compressing objects", CloneStage::Counting),
        ("Receiving objects", CloneStage::Receiving),
        ("Resolving deltas", CloneStage::Resolving),
        ("Updating files", CloneStage::Checkout),
        ("Checking out files", CloneStage::Checkout),
    ];
    let stage = STAGES.iter().find(|(p, _)| t.starts_with(p))?.1;
    // `: <n>% (<a>/<b>)<tail>`
    let parsed = (|| {
        // The first `:<space>` followed by a percentage.
        let after = t.match_indices(':').map(|(i, _)| &t[i + 1..]).find(|rest| {
            rest.starts_with(char::is_whitespace)
                && rest.trim_start().split_once('%').is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })?;
        let (pct, rest) = after.trim_start().split_once('%')?;
        let percent: u64 = pct.trim().parse().ok()?;
        let rest = rest.trim_start().strip_prefix('(')?;
        let (counts, tail) = rest.split_once(')')?;
        let (a, b) = counts.split_once('/')?;
        a.parse::<u64>().ok()?;
        b.parse::<u64>().ok()?;
        let mut tail = tail.trim_end().to_owned();
        if let Some(stripped) = tail.strip_suffix(", done.") {
            tail = stripped.trim_end().to_owned();
        }
        if !tail.is_empty() && !tail.starts_with(',') {
            return Some((None, None));
        }
        let detail = tail.get(1..).unwrap_or("").trim().to_owned();
        let detail = (!detail.is_empty() && detail != "done.").then_some(detail);
        Some((Some(percent.min(100)), detail))
    })();
    match parsed {
        Some((percent, detail)) => Some((stage, percent, detail)),
        None => Some((stage, None, None)),
    }
}

fn redact_url_credentials(text: &str) -> String {
    text.split(' ')
        .map(|w| if w.contains("://") { redact_remote_url(w.trim_matches(['\'', '"'])) } else { w.to_owned() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why `git clone` did not finish.
#[derive(Debug, thiserror::Error)]
#[error("{detail}")]
struct CloneFailure {
    /// What git said last, or what went wrong running it.
    detail: String,
    /// The host refused the key or its own was not known: https may still work.
    ssh_rejected: bool,
}

/// Runs `git clone`; `on_progress` sees parsed progress, `on_stall` fires after 20s of silence.
async fn run_clone(
    destination: &Path,
    clone_url: &str,
    timeout: Option<Duration>,
    mut on_progress: impl FnMut(CloneStage, Option<u64>, Option<String>),
    mut on_stall: impl FnMut(),
) -> Result<(), CloneFailure> {
    let parent = destination.parent().unwrap_or(Path::new("/"));
    let name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut child = crate::process::command("git")
        .args(["clone", "--progress", "--", clone_url, &name])
        .current_dir(parent)
        .env("GIT_PROGRESS_DELAY", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LANGUAGE", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CloneFailure { detail: format!("The repository could not be cloned: {e}"), ssh_rejected: false })?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut tail: Vec<String> = vec![];
    let mut all = String::new();
    let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
    let mut buf = [0u8; 4096];
    let mut pending = String::new();
    loop {
        if let (Some(d), Some(t)) = (deadline, timeout) && tokio::time::Instant::now() >= d {
            let secs = t.as_secs();
            return Err(CloneFailure { detail: format!("The clone timed out after {secs}s."), ssh_rejected: false });
        }
        let read = match tokio::time::timeout(STALL_AFTER, stderr.read(&mut buf)).await {
            Err(_) => {
                on_stall();
                continue;
            }
            Ok(r) => r.unwrap_or(0),
        };
        if read == 0 {
            break;
        }
        pending.push_str(&String::from_utf8_lossy(&buf[..read]));
        while let Some(i) = pending.find(['\r', '\n']) {
            let line: String = pending.drain(..=i).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            all.push_str(line);
            all.push('\n');
            if let Some((stage, percent, detail)) = parse_clone_progress(line) {
                on_progress(stage, percent, detail);
            } else {
                let t = line.trim();
                if !t.is_empty() && !t.starts_with("Cloning into") {
                    tail.push(redact_url_credentials(t));
                    if tail.len() > 4 {
                        tail.remove(0);
                    }
                }
            }
        }
    }
    let status = child.wait().await.map_err(|e| CloneFailure { detail: e.to_string(), ssh_rejected: false })?;
    if status.success() {
        return Ok(());
    }
    let lower = all.to_lowercase();
    Err(CloneFailure {
        detail: if tail.is_empty() { "The repository could not be cloned.".into() } else { tail.join(" ") },
        ssh_rejected: lower.contains("permission denied (publickey") || lower.contains("host key verification failed"),
    })
}

fn is_ssh_url(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && url.contains('@') && url.contains(':'))
}

/// The HTTPS form of an SSH clone URL, when SSH may fall back (t3code's `httpsFallbackUrl`).
fn https_fallback(protocol: Option<CloneProtocol>, prepared: &PreparedClone) -> Option<String> {
    if protocol == Some(CloneProtocol::Ssh) || !is_ssh_url(&prepared.clone_url) {
        return None;
    }
    if let Some(repository) = prepared.repository.as_ref().filter(|r| r.ssh_url == prepared.clone_url) {
        return Some(repository.url.clone());
    }
    let (host, path) = crate::vcs::parse_remote(&prepared.clone_url).map(|(h, o, n)| (h, format!("{o}/{n}")))?;
    Some(format!("https://{host}/{path}.git"))
}

/// Clones with SSH→HTTPS fallback; returns the (redacted) remote URL used.
async fn clone_with_fallback(
    protocol: Option<CloneProtocol>,
    prepared: &PreparedClone,
    timeout: Option<Duration>,
    on_progress: impl FnMut(CloneStage, Option<u64>, Option<String>) + Clone,
    on_stall: impl FnMut() + Clone,
) -> Result<String, CloneFailure> {
    match run_clone(&prepared.destination, &prepared.clone_url, timeout, on_progress.clone(), on_stall.clone()).await {
        Ok(()) => Ok(prepared.remote_url.clone()),
        Err(first) => match https_fallback(protocol, prepared).filter(|_| first.ssh_rejected) {
            None => Err(first),
            Some(https) => {
                let _ = discard_clone(&prepared.destination);
                run_clone(&prepared.destination, &https, timeout, on_progress, on_stall).await.map(|_| redact_remote_url(&https)).map_err(|second| CloneFailure {
                    detail: format!("{} Cloning over HTTPS instead also failed: {}", first.detail, second.detail),
                    ssh_rejected: first.ssh_rejected,
                })
            }
        },
    }
}

/// The result of `sourceControl.cloneRepository` (`SourceControlCloneRepositoryResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloneResult {
    cwd: String,
    /// Where the clone came from, without credentials.
    remote_url: String,
    /// `null` for a clone asked for by URL.
    repository: Option<RepositoryInfo>,
}

async fn clone_repository(input: CloneInput) -> Result<CloneResult, Failure> {
    let provider = input.provider.clone().unwrap_or_else(|| "unknown".into());
    let prepared = prepare_clone(&input).await?;
    let remote_url = clone_with_fallback(input.protocol, &prepared, Some(CLONE_TIMEOUT), |_, _, _| {}, || {})
        .await
        .map_err(|e| Failure::repository(&provider, Operation::CloneRepository, e.detail))?;
    Ok(CloneResult { cwd: prepared.destination.to_string_lossy().into_owned(), remote_url, repository: prepared.repository })
}

/// Removes a partial clone, keeping the (empty) destination folder.
fn discard_clone(destination: &Path) -> Result<(), Failure> {
    let fail = |detail: &str| Failure::repository("unknown", Operation::DiscardClone, detail);
    let entries: Vec<String> = match std::fs::read_dir(destination) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(_) => return Err(fail("The clone destination could not be inspected.")),
    };
    if !entries.is_empty() && !entries.iter().any(|e| e == ".git") {
        return Err(fail("Destination path contains files that are not from the clone."));
    }
    let _ = std::fs::remove_dir_all(destination);
    std::fs::create_dir_all(destination).map_err(|_| fail("The partial clone could not be removed."))
}

/// Who may see a published repository (`SourceControlRepositoryVisibility`).
/// Any text but `public` is private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Private,
    Public,
}

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D: serde::Deserializer<'de>>(sent: D) -> Result<Self, D::Error> {
        Ok(if String::deserialize(sent)? == "public" { Visibility::Public } else { Visibility::Private })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishInput {
    cwd: String,
    provider: String,
    repository: String,
    visibility: Visibility,
    #[serde(default)]
    remote_name: Option<String>,
    #[serde(default)]
    protocol: Option<CloneProtocol>,
}

/// How far a publish got (`SourceControlPublishStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublishStatus {
    Pushed,
    /// The checkout has no commit yet: there was nothing to push.
    RemoteAdded,
}

/// The result of `sourceControl.publishRepository` (`SourceControlPublishRepositoryResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishResult {
    repository: RepositoryInfo,
    remote_name: String,
    remote_url: String,
    branch: String,
    status: PublishStatus,
}

async fn git_out(cwd: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::process::git(cwd);
    cmd.args(args);
    let out = crate::process::output(cmd, crate::process::SLOW).await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Adds `url` as a remote, reusing one that already points at it.
async fn ensure_remote(cwd: &str, preferred: &str, url: &str) -> Result<String, String> {
    let remotes = git_out(cwd, &["remote"]).await?;
    let names: Vec<&str> = remotes.lines().collect();
    for name in &names {
        if git_out(cwd, &["remote", "get-url", name]).await.is_ok_and(|u| u.trim() == url) {
            return Ok((*name).to_owned());
        }
    }
    let mut name = preferred.to_owned();
    let mut n = 2;
    while names.contains(&name.as_str()) {
        name = format!("{preferred}-{n}");
        n += 1;
    }
    git_out(cwd, &["remote", "add", &name, url]).await?;
    Ok(name)
}

async fn publish_repository(input: PublishInput) -> Result<PublishResult, Failure> {
    let provider = input.provider.as_str();
    if provider == "unknown" {
        return Err(Failure::repository(provider, Operation::PublishRepository, "Choose a source control provider before continuing."));
    }
    if provider != "github" {
        return Err(Failure::repository(provider, Operation::PublishRepository, GENERIC_DETAIL));
    }
    let fail = |detail: String| Failure::repository(provider, Operation::PublishRepository, detail);
    let repository = input.repository.trim();
    let visibility = if input.visibility == Visibility::Public { "--public" } else { "--private" };
    gh::run(Some(&input.cwd), "github.com", &["repo", "create", repository, visibility], None).await.map_err(|e| fail(e.detail))?;
    let info = view_repository(provider, repository, Some(&input.cwd)).await.map_err(|e| fail(e.detail))?;
    let mut remote_url = select_remote_url(&info, input.protocol);
    let preferred = input.remote_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("origin");
    let remote_name = ensure_remote(&input.cwd, preferred, &remote_url).await.map_err(fail)?;
    let branch = git_out(&input.cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.map(|s| s.trim().to_owned()).ok();
    if git_out(&input.cwd, &["rev-parse", "--verify", "HEAD"]).await.is_err() {
        return Ok(PublishResult {
            repository: info,
            remote_name,
            remote_url,
            branch: branch.unwrap_or_else(|| "main".into()),
            status: PublishStatus::RemoteAdded,
        });
    }
    let Some(branch) = branch else { return Err(fail("Cannot push from detached HEAD.".into())) };
    let created = format!("Created {} and added it as remote \"{remote_name}\", but the push failed", info.name_with_owner);
    let push = |cwd: String, remote: String, branch: String| async move {
        git_out(&cwd, &["push", "-u", "--", &remote, &format!("HEAD:refs/heads/{branch}")]).await
    };
    if let Err(stderr) = push(input.cwd.clone(), remote_name.clone(), branch.clone()).await {
        let ssh_rejected = stderr.to_lowercase().contains("permission denied (publickey") || stderr.contains("Host key verification failed");
        let https = info.url.clone();
        if ssh_rejected && input.protocol != Some(CloneProtocol::Ssh) && remote_url == info.ssh_url {
            git_out(&input.cwd, &["remote", "set-url", &remote_name, &https]).await.map_err(fail)?;
            remote_url = https;
            push(input.cwd.clone(), remote_name.clone(), branch.clone()).await.map_err(|_| fail(format!("{created}. Fix the problem, then push again.")))?;
        } else {
            return Err(fail(format!("{created}. Fix the problem, then push again.")));
        }
    }
    Ok(PublishResult { repository: info, remote_name, remote_url, branch, status: PublishStatus::Pushed })
}

/// `user/starred/{owner}/{name}` for a plain `owner/name`.
fn starred_path(repository: &str) -> Option<String> {
    let (owner, name) = repository.trim().split_once('/')?;
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok(owner) && ok(name)).then(|| format!("user/starred/{owner}/{name}"))
}

#[derive(Deserialize)]
struct StarInput {
    provider: String,
    host: String,
    repository: String,
}

async fn starred(input: StarInput, star: bool) -> bool {
    if input.provider != "github" {
        return false;
    }
    let Some(path) = starred_path(&input.repository) else { return false };
    let args: Vec<&str> = if star { vec!["api", "-X", "PUT", &path, "--silent"] } else { vec!["api", &path, "--silent"] };
    gh::run(None, &input.host, &args, None).await.is_ok()
}

// ---------------------------------------------------------------------------
// Project clones
// ---------------------------------------------------------------------------

/// Whether a tracked clone still runs, and how it ended (`ProjectClonePhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ClonePhase {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// A project's clone as clients see it (`ProjectCloneSnapshot`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloneSnapshot {
    project_id: ProjectId,
    /// Where the clone comes from, without credentials.
    remote_url: String,
    destination_path: String,
    /// `null` for a clone asked for by URL.
    repository: Option<RepositoryInfo>,
    phase: ClonePhase,
    stage: CloneStage,
    /// How far the stage is, 0 to 100, when git says.
    percent: Option<u64>,
    /// What git adds to the percentage (sizes, speed), cut to `DETAIL_MAX` characters.
    detail: Option<String>,
    /// Git has printed nothing for a while.
    stalled: bool,
    /// Why the clone failed, cut to `ERROR_MAX` characters.
    error: Option<String>,
    started_at: Timestamp,
    ended_at: Option<Timestamp>,
    /// Grows with every change to any clone: the newer of two snapshots has the higher one.
    sequence: u64,
}

struct Tracked {
    project_id: String,
    snapshot: CloneSnapshot,
    task: Option<tokio::task::AbortHandle>,
    clone_url: String,
    destination: PathBuf,
    protocol: Option<CloneProtocol>,
}

#[derive(Default)]
struct Tracker {
    clones: Vec<Tracked>,
    sequence: u64,
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(Default::default);
static CHANGES: LazyLock<watch::Sender<Vec<CloneSnapshot>>> = LazyLock::new(|| watch::channel(vec![]).0);
static ACTIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn snapshots(t: &Tracker) -> Vec<CloneSnapshot> {
    t.clones.iter().map(|c| c.snapshot.clone()).collect()
}

fn publish(t: &Tracker) {
    CHANGES.send_replace(snapshots(t));
}

/// Applies `f` to a tracked clone's snapshot, bumps its sequence and publishes.
fn modify(project_id: &str, f: impl FnOnce(&mut CloneSnapshot)) {
    let mut t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    t.sequence += 1;
    let seq = t.sequence;
    let Some(tracked) = t.clones.iter_mut().find(|c| c.project_id == project_id) else { return };
    f(&mut tracked.snapshot);
    tracked.snapshot.sequence = seq;
    publish(&t);
}

fn get(project_id: &str) -> Option<CloneSnapshot> {
    TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clones.iter().find(|c| c.project_id == project_id).map(|c| c.snapshot.clone())
}

fn remove(project_id: &str) {
    let mut t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    t.clones.retain(|c| c.project_id != project_id);
    publish(&t);
}

fn finish(project_id: &str, phase: ClonePhase, error: Option<String>) {
    let ended = Timestamp::from(domovoi_contracts::iso_now());
    modify(project_id, |s| {
        s.phase = phase;
        s.ended_at = Some(ended);
        if phase == ClonePhase::Done {
            s.percent = Some(100);
        }
        s.stalled = false;
        s.error = error.map(|e| clamp(&e, ERROR_MAX));
    });
    if let Some(c) = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clones.iter_mut().find(|c| c.project_id == project_id) {
        c.task = None;
    }
    if phase == ClonePhase::Done {
        let id = project_id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(DONE_RETENTION).await;
            if get(&id).is_some_and(|s| s.phase == ClonePhase::Done) {
                remove(&id);
            }
        });
    }
}

/// Re-reads the cloned project's repository identity (t3code's `onCloned`).
async fn on_cloned(state: &AppState, project_id: &str, root: &str) {
    let Ok(Some(mut project)) = state.store.get_project(project_id) else { return };
    project.repository_identity = crate::vcs::repository_identity(root).await;
    project.updated_at = domovoi_contracts::iso_now();
    let json = serde_json::to_string(&project).unwrap_or_default();
    let id = project.id.clone();
    let _ = state.store.append("project", &id, "project.updated", &json, None, |tx| {
        tx.execute("UPDATE projects SET shell = ?2 WHERE id = ?1", rusqlite::params![id, json]).map(|_| ())
    });
}

fn launch(state: Arc<AppState>, project_id: String) {
    let Some((clone_url, destination, protocol, repository)) = crate::util::locked(&TRACKER)
        .clones
        .iter()
        .find(|c| c.project_id == project_id)
        .map(|c| (c.clone_url.clone(), c.destination.clone(), c.protocol, c.snapshot.repository.clone()))
    else {
        return;
    };
    let id = project_id.clone();
    let handle = tokio::spawn(async move {
        let prepared = PreparedClone { destination: destination.clone(), remote_url: redact_remote_url(&clone_url), clone_url, repository };
        let progress_id = id.clone();
        let on_progress = move |stage: CloneStage, percent: Option<u64>, detail: Option<String>| {
            modify(&progress_id, |s| {
                s.stage = stage;
                s.percent = percent;
                s.detail = detail.map(|d| clamp(&d, DETAIL_MAX));
                s.stalled = false;
            });
        };
        let stall_id = id.clone();
        let on_stall = move || {
            if get(&stall_id).is_some_and(|s| !s.stalled) {
                modify(&stall_id, |s| s.stalled = true);
            }
        };
        let result = clone_with_fallback(protocol, &prepared, None, on_progress, on_stall).await;
        match result {
            Ok(_) => {
                finish(&id, ClonePhase::Done, None);
                on_cloned(&state, &id, &destination.to_string_lossy()).await;
            }
            Err(e) => finish(&id, ClonePhase::Failed, Some(e.detail)),
        }
    });
    if let Some(c) = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clones.iter_mut().find(|c| c.project_id == project_id) {
        if c.snapshot.phase == ClonePhase::Running {
            c.task = Some(handle.abort_handle());
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloneStartInput {
    project_id: String,
    title: String,
    #[serde(flatten)]
    clone: CloneInput,
}

/// Creates the clone's project; a project already rooted there takes the clone instead.
fn create_project(state: &AppState, project_id: &str, title: &str, root: &Path) -> Result<String, Failure> {
    let dispatch_error = |message: &str| Failure::Dispatch { message: message.to_owned() };
    let root_str = root.to_string_lossy().into_owned();
    if let Ok(projects) = state.store.list_projects() {
        if let Some(existing) = projects.iter().find(|p| p.workspace_root == root_str) {
            return Ok(existing.id.clone());
        }
    }
    std::fs::create_dir_all(root).map_err(|_| dispatch_error("Failed to create clone project."))?;
    let now = domovoi_contracts::iso_now();
    let shell = domovoi_contracts::shell::ProjectShell {
        id: project_id.to_owned(), title: title.to_owned(), has_custom_title: false, workspace_root: root_str,
        repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
        favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
        created_at: now.clone(), updated_at: now,
    };
    let command_id = format!("project-clone-create:{project_id}");
    state.store.create_project(shell, Some(&command_id)).map_err(|_| dispatch_error("Failed to create clone project."))?;
    Ok(project_id.to_owned())
}

/// The result of `projectClone.start` (`ProjectCloneStartResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloneStartResult {
    /// The project the clone belongs to: the one asked for, or the one already on the folder.
    project_id: ProjectId,
    cwd: String,
    /// Where the clone comes from, without credentials.
    remote_url: String,
    /// `null` for a clone asked for by URL.
    repository: Option<RepositoryInfo>,
}

async fn clone_start(state: Arc<AppState>, input: CloneStartInput) -> Result<CloneStartResult, Failure> {
    let _lock = ACTIONS.lock().await;
    let provider = input.clone.provider.clone().unwrap_or_else(|| "unknown".into());
    let prepared = prepare_clone(&input.clone).await?;
    let in_progress = || Failure::repository(&provider, Operation::CloneRepository, "A clone into this destination is already in progress.");
    {
        let mut t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if t.clones.iter().any(|c| c.project_id == input.project_id || c.destination == prepared.destination) {
            return Err(in_progress());
        }
        t.sequence += 1;
        let snapshot = CloneSnapshot {
            project_id: ProjectId::from(input.project_id.as_str()),
            remote_url: prepared.remote_url.clone(),
            destination_path: prepared.destination.to_string_lossy().into_owned(),
            repository: prepared.repository.clone(),
            phase: ClonePhase::Running,
            stage: CloneStage::Connecting,
            percent: None,
            detail: None,
            stalled: false,
            error: None,
            started_at: Timestamp::from(domovoi_contracts::iso_now()),
            ended_at: None,
            sequence: t.sequence,
        };
        t.clones.push(Tracked {
            project_id: input.project_id.clone(), snapshot, task: None, clone_url: prepared.clone_url.clone(),
            destination: prepared.destination.clone(), protocol: input.clone.protocol,
        });
    }
    let project_id = match create_project(&state, &input.project_id, &input.title, &prepared.destination) {
        Ok(id) => id,
        Err(e) => {
            remove(&input.project_id);
            return Err(e);
        }
    };
    if project_id != input.project_id {
        let mut t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if t.clones.iter().any(|c| c.project_id == project_id) {
            t.clones.retain(|c| c.project_id != input.project_id);
            publish(&t);
            return Err(in_progress());
        }
        if let Some(c) = t.clones.iter_mut().find(|c| c.project_id == input.project_id) {
            c.project_id = project_id.clone();
            c.snapshot.project_id = ProjectId::from(project_id.as_str());
        }
    }
    publish(&TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    launch(state, project_id.clone());
    Ok(CloneStartResult {
        project_id: ProjectId::from(project_id),
        cwd: prepared.destination.to_string_lossy().into_owned(),
        remote_url: prepared.remote_url,
        repository: prepared.repository,
    })
}

async fn clone_cancel(project_id: &str) -> bool {
    let _lock = ACTIONS.lock().await;
    let (task, destination) = {
        let t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(c) = t.clones.iter().find(|c| c.project_id == project_id) else { return false };
        if c.snapshot.phase != ClonePhase::Running {
            return false;
        }
        let Some(task) = c.task.clone() else { return false };
        (task, c.destination.clone())
    };
    task.abort();
    if get(project_id).is_some_and(|s| s.phase == ClonePhase::Done) {
        return true;
    }
    if get(project_id).is_some_and(|s| s.phase == ClonePhase::Running) {
        finish(project_id, ClonePhase::Cancelled, None);
    }
    // Give git a moment to exit before removing what it wrote.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = discard_clone(&destination);
    true
}

async fn clone_retry(state: Arc<AppState>, project_id: &str) -> Result<bool, Failure> {
    let _lock = ACTIONS.lock().await;
    let destination = {
        let t = TRACKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(c) = t.clones.iter().find(|c| c.project_id == project_id) else { return Ok(false) };
        if !matches!(c.snapshot.phase, ClonePhase::Failed | ClonePhase::Cancelled) {
            return Ok(false);
        }
        c.destination.clone()
    };
    discard_clone(&destination)?;
    let started = Timestamp::from(domovoi_contracts::iso_now());
    modify(project_id, |s| {
        s.phase = ClonePhase::Running;
        s.stage = CloneStage::Connecting;
        s.percent = None;
        s.detail = None;
        s.stalled = false;
        s.error = None;
        s.started_at = started;
        s.ended_at = None;
    });
    launch(state, project_id.to_owned());
    Ok(true)
}

/// `subscribeProjectClones`: every tracked clone (`ProjectCloneListEvent`), now and after each change.
fn clones_stream() -> BoxStream<'static, Result<Vec<CloneSnapshot>, Never>> {
    let mut rx = CHANGES.subscribe();
    async_stream::stream! {
        let first = rx.borrow_and_update().clone();
        yield Ok(first);
        while rx.changed().await.is_ok() {
            let next = rx.borrow_and_update().clone();
            yield Ok(next);
        }
    }
    .boxed()
}

/// The result of `projectClone.cancel` and `projectClone.retry` (`ProjectCloneActionResult`).
#[derive(Debug, Serialize)]
struct CloneActionResult {
    /// Whether the clone was in a state to be cancelled or retried, and was.
    applied: bool,
}

/// Whether the server's forge app reaches a repository (`SourceControlAppAccess.status`):
/// Domovoi has no forge app, so it is always signed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AppAccessStatus {
    SignedOut,
}

/// The result of `sourceControl.appAccess` (`SourceControlAppAccess`).
#[derive(Debug, Serialize)]
struct AppAccess {
    status: AppAccessStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectIdInput {
    project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LookupInput {
    provider: String,
    repository: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct SignInInput {
    provider: String,
    host: String,
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("sourceControl.lookupRepository", |ctx: Arc<ConnCtx>, input: LookupInput| async move {
        let cwd = input.cwd.unwrap_or_else(|| ctx.state.cwd.clone());
        lookup_repository(&input.provider, &input.repository, Some(&cwd)).await
    })
    .unary("sourceControl.cloneRepository", |_, input: CloneInput| clone_repository(input))
    .unary("sourceControl.publishRepository", |_, input: PublishInput| publish_repository(input))
    // No sign-in ever starts, so none of the three has an answer.
    .unary("sourceControl.signInStart", |_, input: SignInInput| async move { Err::<Never, _>(Failure::sign_in(&input.provider, &input.host)) })
    .unary("sourceControl.signInWait", |_, input: SignInInput| async move { Err::<Never, _>(Failure::sign_in(&input.provider, &input.host)) })
    .unary("sourceControl.signInCancel", |_, input: SignInInput| async move { Err::<Never, _>(Failure::sign_in(&input.provider, &input.host)) })
    .unary("sourceControl.appAccess", |_, _: IgnoredAny| async { Ok::<_, Never>(AppAccess { status: AppAccessStatus::SignedOut }) })
    .unary("sourceControl.starred", |_, input: StarInput| async move { Ok::<_, Never>(starred(input, false).await) })
    .unary("sourceControl.star", |_, input: StarInput| async move { Ok::<_, Never>(starred(input, true).await) })
    .unary("projectClone.start", |ctx: Arc<ConnCtx>, input: CloneStartInput| clone_start(ctx.state.clone(), input))
    .unary("projectClone.cancel", |_, input: ProjectIdInput| async move {
        Ok::<_, Never>(CloneActionResult { applied: clone_cancel(&input.project_id).await })
    })
    .unary("projectClone.retry", |ctx: Arc<ConnCtx>, input: ProjectIdInput| async move {
        Ok::<_, Failure>(CloneActionResult { applied: clone_retry(ctx.state.clone(), &input.project_id).await? })
    })
    .stream("subscribeProjectClones", |_, _: IgnoredAny| clones_stream())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        let operations = [
            (Operation::LookupRepository, "lookupRepository"),
            (Operation::CloneRepository, "cloneRepository"),
            (Operation::PublishRepository, "publishRepository"),
            (Operation::DiscardClone, "discardClone"),
        ];
        for (operation, spelled) in operations {
            assert_eq!(
                json!(Failure::repository("github", operation, "No such repository.")),
                json!({ "_tag": "SourceControlRepositoryError", "provider": "github", "operation": spelled, "detail": "No such repository." })
            );
        }
        assert_eq!(
            json!(Failure::repository("unknown", Operation::LookupRepository, "Choose a source control provider before continuing.").during(Operation::CloneRepository)),
            json!({ "_tag": "SourceControlRepositoryError", "provider": "unknown", "operation": "cloneRepository", "detail": "Choose a source control provider before continuing." })
        );
        assert_eq!(
            json!(Failure::sign_in("gitlab", "gitlab.example.com")),
            json!({
                "_tag": "SourceControlSignInError", "provider": "gitlab", "host": "gitlab.example.com", "reason": "unsupported",
                "detail": "This server cannot sign in to gitlab. Use a token or the host's CLI.",
            })
        );
        assert_eq!(
            json!(Failure::Dispatch { message: "Failed to create clone project.".into() }),
            json!({ "_tag": "OrchestrationDispatchCommandError", "message": "Failed to create clone project." })
        );
    }

    #[test]
    fn answers_are_written_as_the_records_of_t3code() {
        let repository = RepositoryInfo { provider: "github".into(), name_with_owner: "o/r".into(), url: "https://github.com/o/r".into(), ssh_url: "git@github.com:o/r.git".into() };
        let repository_json = json!({ "provider": "github", "nameWithOwner": "o/r", "url": "https://github.com/o/r", "sshUrl": "git@github.com:o/r.git" });
        assert_eq!(json!(repository), repository_json);
        assert_eq!(
            json!(CloneResult { cwd: "/w/r".into(), remote_url: "/src/r".into(), repository: None }),
            json!({ "cwd": "/w/r", "remoteUrl": "/src/r", "repository": null })
        );
        assert_eq!(
            json!(PublishResult { repository: repository.clone(), remote_name: "origin".into(), remote_url: "git@github.com:o/r.git".into(), branch: "main".into(), status: PublishStatus::RemoteAdded }),
            json!({ "repository": repository_json, "remoteName": "origin", "remoteUrl": "git@github.com:o/r.git", "branch": "main", "status": "remote_added" })
        );
        assert_eq!(json!(PublishStatus::Pushed), json!("pushed"));
        assert_eq!(
            json!(CloneStartResult { project_id: ProjectId::from("p1"), cwd: "/w/r".into(), remote_url: "git@github.com:o/r.git".into(), repository: Some(repository.clone()) }),
            json!({ "projectId": "p1", "cwd": "/w/r", "remoteUrl": "git@github.com:o/r.git", "repository": repository_json })
        );
        let running = CloneSnapshot {
            project_id: ProjectId::from("p1"),
            remote_url: "git@github.com:o/r.git".into(),
            destination_path: "/w/r".into(),
            repository: None,
            phase: ClonePhase::Running,
            stage: CloneStage::Connecting,
            percent: None,
            detail: None,
            stalled: false,
            error: None,
            started_at: Timestamp::from("2026-01-01T00:00:00.000Z"),
            ended_at: None,
            sequence: 1,
        };
        assert_eq!(
            json!(running),
            json!({
                "projectId": "p1", "remoteUrl": "git@github.com:o/r.git", "destinationPath": "/w/r", "repository": null,
                "phase": "running", "stage": "connecting", "percent": null, "detail": null, "stalled": false, "error": null,
                "startedAt": "2026-01-01T00:00:00.000Z", "endedAt": null, "sequence": 1,
            })
        );
        let failed = CloneSnapshot {
            repository: Some(repository),
            phase: ClonePhase::Failed,
            stage: CloneStage::Receiving,
            percent: Some(45),
            detail: Some("1.20 MiB | 2.00 MiB/s".into()),
            stalled: true,
            error: Some("fatal: early EOF".into()),
            ended_at: Some(Timestamp::from("2026-01-01T00:00:09.000Z")),
            sequence: 7,
            ..running
        };
        assert_eq!(
            json!(failed),
            json!({
                "projectId": "p1", "remoteUrl": "git@github.com:o/r.git", "destinationPath": "/w/r", "repository": repository_json,
                "phase": "failed", "stage": "receiving", "percent": 45, "detail": "1.20 MiB | 2.00 MiB/s", "stalled": true,
                "error": "fatal: early EOF", "startedAt": "2026-01-01T00:00:00.000Z", "endedAt": "2026-01-01T00:00:09.000Z", "sequence": 7,
            })
        );
        assert_eq!(json!([ClonePhase::Done, ClonePhase::Cancelled]), json!(["done", "cancelled"]));
        assert_eq!(json!([CloneStage::Counting, CloneStage::Resolving, CloneStage::Checkout]), json!(["counting", "resolving", "checkout"]));
        assert_eq!(json!(CloneActionResult { applied: true }), json!({ "applied": true }));
        assert_eq!(json!(AppAccess { status: AppAccessStatus::SignedOut }), json!({ "status": "signed-out" }));
    }

    #[test]
    fn inputs_take_any_text_where_the_schema_names_a_few() {
        let clone: CloneInput = serde_json::from_value(json!({ "destinationPath": "/w/r", "protocol": "https" })).unwrap();
        assert_eq!(clone.protocol, Some(CloneProtocol::Https));
        let clone: CloneInput = serde_json::from_value(json!({ "destinationPath": "/w/r", "protocol": "ssh" })).unwrap();
        assert_eq!(clone.protocol, Some(CloneProtocol::Ssh));
        let clone: CloneInput = serde_json::from_value(json!({ "destinationPath": "/w/r", "protocol": "carrier pigeon" })).unwrap();
        assert_eq!(clone.protocol, Some(CloneProtocol::Auto));
        let clone: CloneInput = serde_json::from_value(json!({ "destinationPath": "/w/r", "protocol": null })).unwrap();
        assert_eq!(clone.protocol, None);
        assert!(serde_json::from_value::<CloneInput>(json!({ "destinationPath": "/w/r", "protocol": 1 })).is_err());
        // The same through the flattened input of `projectClone.start`.
        let start: CloneStartInput = serde_json::from_value(json!({ "projectId": "p1", "title": "r", "destinationPath": "/w/r", "protocol": "https" })).unwrap();
        assert_eq!(start.clone.protocol, Some(CloneProtocol::Https));
        let publish = |visibility: &str| {
            serde_json::from_value::<PublishInput>(json!({ "cwd": "/w/r", "provider": "github", "repository": "o/r", "visibility": visibility })).unwrap().visibility
        };
        assert_eq!(publish("public"), Visibility::Public);
        assert_eq!(publish("private"), Visibility::Private);
        assert_eq!(publish("internal"), Visibility::Private);
    }

    #[test]
    fn parses_clone_progress_like_t3code() {
        assert_eq!(
            parse_clone_progress("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            Some((CloneStage::Receiving, Some(45), Some("1.20 MiB | 2.00 MiB/s".into())))
        );
        assert_eq!(parse_clone_progress("Resolving deltas: 100% (10/10), done."), Some((CloneStage::Resolving, Some(100), None)));
        assert_eq!(parse_clone_progress("remote: Counting objects: 5% (1/20)"), Some((CloneStage::Counting, Some(5), None)));
        assert_eq!(parse_clone_progress("Receiving objects: 12% (1/8) junk"), Some((CloneStage::Receiving, None, None)));
        assert_eq!(parse_clone_progress("Updating files"), Some((CloneStage::Checkout, None, None)));
        assert_eq!(parse_clone_progress("Cloning into 'x'..."), None);
    }

    #[test]
    fn redacts_credentials() {
        assert_eq!(redact_remote_url("https://user:tok@github.com/o/r.git"), "https://github.com/o/r.git");
        assert_eq!(redact_remote_url("https://github.com/o/r.git?token=x"), "https://github.com/o/r.git");
        assert_eq!(redact_remote_url("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(redact_url_credentials("fatal: 'https://a:b@h/x' not found"), "fatal: https://h/x not found");
    }

    // Unix paths.
    #[cfg(unix)]
    #[test]
    fn resolves_paths_lexically() {
        assert_eq!(resolve_path("/a/b/../c/./d"), PathBuf::from("/a/c/d"));
        assert!(resolve_path("~/x").starts_with(crate::util::home_dir()));
    }

    #[test]
    fn falls_back_to_https_only_for_ssh() {
        let prepared = |url: &str| PreparedClone { destination: "/x".into(), remote_url: url.into(), clone_url: url.into(), repository: None };
        assert_eq!(https_fallback(None, &prepared("git@github.com:o/r.git")), Some("https://github.com/o/r.git".into()));
        assert_eq!(https_fallback(Some(CloneProtocol::Auto), &prepared("git@github.com:o/r.git")), Some("https://github.com/o/r.git".into()));
        assert_eq!(https_fallback(Some(CloneProtocol::Ssh), &prepared("git@github.com:o/r.git")), None);
        assert_eq!(https_fallback(None, &prepared("https://github.com/o/r.git")), None);
        // A repository found by name falls back to the https URL its host gave.
        let named = PreparedClone {
            repository: Some(RepositoryInfo { provider: "github".into(), name_with_owner: "o/r".into(), url: "https://github.com/o/r".into(), ssh_url: "git@github.com:o/r.git".into() }),
            ..prepared("git@github.com:o/r.git")
        };
        assert_eq!(https_fallback(None, &named), Some("https://github.com/o/r".into()));
        assert_eq!(starred_path("o/r"), Some("user/starred/o/r".into()));
        assert_eq!(starred_path("not a repo"), None);
    }

    #[tokio::test]
    async fn clones_a_local_repository_and_refuses_busy_destinations() {
        let root = std::env::temp_dir().join(format!("domovoi-clone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src");
        std::fs::create_dir_all(&source).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]] {
            assert!(std::process::Command::new("git").args(args).current_dir(&source).status().unwrap().success());
        }
        let input = |dest: &str| CloneInput {
            remote_url: Some(source.to_string_lossy().into_owned()),
            destination_path: root.join(dest).to_string_lossy().into_owned(),
            ..Default::default()
        };
        let cloned = clone_repository(input("copy")).await.unwrap();
        assert!(root.join("copy/.git").exists());
        assert_eq!(cloned.repository, None);
        let busy = clone_repository(input("copy")).await.unwrap_err();
        assert!(matches!(&busy, Failure::Repository { detail, .. } if detail == "Destination path already exists and is not empty."), "{busy:?}");
        let missing = clone_repository(CloneInput { remote_url: Some(root.join("nope").to_string_lossy().into_owned()), ..input("other") }).await.unwrap_err();
        assert!(matches!(missing, Failure::Repository { operation: Operation::CloneRepository, .. }), "{missing:?}");
        assert!(discard_clone(&root.join("copy")).is_ok());
        assert_eq!(std::fs::read_dir(root.join("copy")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
