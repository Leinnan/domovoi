//! Git queries behind the `vcs.*` / `subscribeVcs*` RPCs, via the `git` CLI.

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use domovoi_model::typed::{
    RepositoryIdentity, RepositoryIdentityLocator, SourceControlProviderInfo, VcsBranchChanges, VcsBranchStatus, VcsListRefsResult, VcsRef,
    VcsStatusLocal, VcsStatusRemote, VcsWorkingTree, VcsWorkingTreeFile,
};

/// These are quick reads. One that takes this long is stuck, most often on
/// a macOS prompt for access to the folder (Documents, Desktop) that nobody
/// at the Mac has answered; giving up keeps it from holding up the caller.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub async fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let run = crate::process::command("git").args(args).current_dir(cwd).env("GIT_OPTIONAL_LOCKS", "0").kill_on_drop(true).output();
    let out = match tokio::time::timeout(GIT_TIMEOUT, run).await {
        Ok(out) => out.ok()?,
        Err(_) => {
            tracing::warn!("git {} in {cwd} took over {}s; gave up (is a macOS folder access prompt waiting on the Mac?)", args.join(" "), GIT_TIMEOUT.as_secs());
            return None;
        }
    };
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `path` is a folder, without blocking the caller on a stuck
/// filesystem call (see `GIT_TIMEOUT`).
pub async fn is_dir(path: &str) -> bool {
    let path = path.to_owned();
    let check = tokio::task::spawn_blocking(move || Path::new(&path).is_dir());
    matches!(tokio::time::timeout(GIT_TIMEOUT, check).await, Ok(Ok(true)))
}

pub async fn is_repo(cwd: &str) -> bool {
    is_dir(cwd).await && git(cwd, &["rev-parse", "--is-inside-work-tree"]).await.is_some_and(|s| s.trim() == "true")
}

/// `VcsBranchStatus`.
pub async fn branch_status(cwd: &str) -> VcsBranchStatus {
    if !is_repo(cwd).await {
        return VcsBranchStatus { ref_name: None, is_repo: Some(false) };
    }
    let name = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.map(|s| s.trim().to_owned());
    VcsBranchStatus { ref_name: name.filter(|s| !s.is_empty()), is_repo: Some(true) }
}

async fn primary_remote(cwd: &str) -> Option<(String, String)> {
    let remotes = git(cwd, &["remote"]).await?;
    let name = remotes
        .lines()
        .find(|r| *r == "origin")
        .or_else(|| remotes.lines().next())?
        .to_owned();
    let url = git(cwd, &["remote", "get-url", &name]).await?.trim().to_owned();
    Some((name, url))
}

async fn default_branch(cwd: &str, remote: Option<&str>) -> Option<String> {
    if let Some(remote) = remote {
        if let Some(head) = git(cwd, &["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{remote}/HEAD")]).await {
            return head.trim().rsplit_once('/').map(|(_, b)| b.to_owned());
        }
    }
    None
}

/// Parsed host/owner/name for common remote URL forms.
pub fn parse_remote(url: &str) -> Option<(String, String, String)> {
    let rest = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split_once("://")
        .map(|(_, r)| r.split_once('@').map_or(r, |(_, h)| h).to_owned())
        .or_else(|| url.split_once('@').map(|(_, r)| r.replacen(':', "/", 1).trim_end_matches(".git").to_owned()))?;
    let mut parts = rest.splitn(2, '/');
    let host = parts.next()?.split(':').next()?.to_owned();
    let path = parts.next()?;
    let (owner, name) = path.rsplit_once('/')?;
    Some((host, owner.to_owned(), name.to_owned()))
}

fn provider_kind(host: &str) -> Option<(&'static str, &'static str)> {
    match host {
        "github.com" => Some(("github", "GitHub")),
        "gitlab.com" => Some(("gitlab", "GitLab")),
        "bitbucket.org" => Some(("bitbucket", "Bitbucket")),
        _ => None,
    }
}

/// `RepositoryIdentity` for a workspace, or none when it has no recognizable remote.
pub async fn repository_identity(cwd: &str) -> Option<RepositoryIdentity> {
    if !is_repo(cwd).await {
        return None;
    }
    let (remote_name, remote_url) = primary_remote(cwd).await?;
    let (host, owner, name) = parse_remote(&remote_url)?;
    let root = git(cwd, &["rev-parse", "--show-toplevel"]).await.map(|s| s.trim().to_owned());
    Some(RepositoryIdentity {
        canonical_key: format!("{host}/{owner}/{name}").to_lowercase(),
        locator: RepositoryIdentityLocator { source: "git-remote".into(), remote_name, remote_url },
        web_url: None,
        root_path: root,
        display_name: Some(format!("{owner}/{name}")),
        provider: provider_kind(&host).map(|(kind, _)| kind.to_owned()),
        owner: Some(owner),
        name: Some(name),
        origin: None,
    })
}

async fn numstat(cwd: &str, args: &[&str]) -> Vec<(String, u64, u64)> {
    let Some(out) = git(cwd, args).await else { return vec![] };
    out.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let ins = it.next()?.parse().unwrap_or(0);
            let del = it.next()?.parse().unwrap_or(0);
            Some((it.next()?.to_owned(), ins, del))
        })
        .collect()
}

/// `VcsStatusLocalResult`.
pub async fn local_status(cwd: &str) -> VcsStatusLocal {
    if !is_repo(cwd).await {
        return VcsStatusLocal {
            is_repo: false,
            source_control_provider: None,
            has_primary_remote: false,
            is_default_ref: false,
            ref_name: None,
            has_working_tree_changes: false,
            working_tree: VcsWorkingTree { root_path: None, files: vec![], insertions: 0, deletions: 0 },
            branch_changes: None,
        };
    }
    let root = git(cwd, &["rev-parse", "--show-toplevel"]).await.map(|s| s.trim().to_owned()).unwrap_or_else(|| cwd.into());
    let ref_name = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.map(|s| s.trim().to_owned());
    let remote = primary_remote(cwd).await;
    let default = default_branch(cwd, remote.as_ref().map(|r| r.0.as_str())).await;
    let has_head = git(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_some();

    let mut files: HashMap<String, (u64, u64)> = HashMap::new();
    if has_head {
        for (p, i, d) in numstat(&root, &["diff", "--numstat", "HEAD"]).await {
            files.insert(p, (i, d));
        }
    }
    if let Some(untracked) = git(&root, &["ls-files", "--others", "--exclude-standard"]).await {
        for p in untracked.lines().filter(|l| !l.is_empty()) {
            let lines = std::fs::read_to_string(Path::new(&root).join(p)).map(|s| s.lines().count() as u64).unwrap_or(0);
            files.entry(p.to_owned()).or_insert((lines, 0));
        }
    }
    let mut list: Vec<_> = files.into_iter().collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    let (ins, del) = list.iter().fold((0, 0), |(i, d), (_, (a, b))| (i + a, d + b));
    let has_working_tree_changes = !list.is_empty();
    let files: Vec<VcsWorkingTreeFile> =
        list.into_iter().map(|(path, (insertions, deletions))| VcsWorkingTreeFile { path, insertions, deletions }).collect();
    let provider = remote.as_ref().and_then(|r| parse_remote(&r.1)).and_then(|(h, _, _)| provider_kind(&h));

    VcsStatusLocal {
        is_repo: true,
        source_control_provider: provider.map(|(kind, label)| SourceControlProviderInfo {
            kind: kind.to_owned(),
            name: label.to_owned(),
            base_url: format!("https://{}", match kind { "github" => "github.com", "gitlab" => "gitlab.com", _ => "bitbucket.org" }),
        }),
        has_primary_remote: remote.is_some(),
        is_default_ref: match (&ref_name, &default) {
            (Some(r), Some(d)) => r == d,
            (Some(r), None) => r == "main" || r == "master",
            _ => false,
        },
        ref_name,
        has_working_tree_changes,
        working_tree: VcsWorkingTree { root_path: Some(root), files, insertions: ins, deletions: del },
        branch_changes: Some(VcsBranchChanges {
            base_ref: default.as_ref().zip(remote.as_ref()).map(|(d, r)| format!("{}/{d}", r.0)),
            insertions: ins,
            deletions: del,
        }),
    }
}

/// `VcsStatusRemoteResult` (none when not a repo).
pub async fn remote_status(cwd: &str) -> Option<VcsStatusRemote> {
    if !is_repo(cwd).await {
        return None;
    }
    let upstream = git(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).await;
    let (ahead, behind) = match &upstream {
        Some(_) => git(cwd, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
            .await
            .and_then(|s| {
                let mut it = s.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .unwrap_or((0u64, 0u64)),
        None => (0, 0),
    };
    Some(VcsStatusRemote { has_upstream: upstream.is_some(), ahead_count: ahead, behind_count: behind, ahead_of_default_count: Some(0), pr: None })
}

/// `VcsListRefsResult`.
pub async fn list_refs(cwd: &str, query: Option<&str>, limit: usize) -> VcsListRefsResult {
    if !is_repo(cwd).await {
        return VcsListRefsResult { is_at_repository_root: None, refs: vec![], is_repo: false, has_primary_remote: false, next_cursor: None, total_count: 0 };
    }
    let current = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.map(|s| s.trim().to_owned());
    let remote = primary_remote(cwd).await;
    let default = default_branch(cwd, remote.as_ref().map(|r| r.0.as_str())).await;
    let mut worktrees: HashMap<String, String> = HashMap::new();
    if let Some(out) = git(cwd, &["worktree", "list", "--porcelain"]).await {
        let mut path = None;
        for line in out.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.to_owned());
            } else if let (Some(b), Some(p)) = (line.strip_prefix("branch refs/heads/"), &path) {
                worktrees.insert(b.to_owned(), p.clone());
            }
        }
    }
    let out = git(cwd, &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", "refs/heads", "refs/remotes"])
        .await
        .unwrap_or_default();
    let mut refs = Vec::new();
    for full in out.lines() {
        let (name, is_remote, remote_name) = if let Some(b) = full.strip_prefix("refs/heads/") {
            (b.to_owned(), false, None)
        } else if let Some(r) = full.strip_prefix("refs/remotes/") {
            if r.ends_with("/HEAD") {
                continue;
            }
            (r.to_owned(), true, r.split_once('/').map(|(n, _)| n.to_owned()))
        } else {
            continue;
        };
        if query.is_some_and(|q| !name.to_lowercase().contains(&q.to_lowercase())) {
            continue;
        }
        refs.push(VcsRef {
            current: !is_remote && current.as_deref() == Some(&name),
            is_default: !is_remote && default.as_deref() == Some(&name),
            worktree_path: if is_remote { None } else { worktrees.get(&name).cloned() },
            name,
            is_remote: Some(is_remote),
            upstream_ref: remote_name.as_ref().map(|_| full.to_owned()),
            remote_name,
        });
    }
    let total = refs.len();
    refs.truncate(limit);
    let top = git(cwd, &["rev-parse", "--show-toplevel"]).await.map(|s| s.trim().to_owned());
    let at_root = top.as_deref().map(|t| Path::new(t) == Path::new(cwd)).unwrap_or(false);
    VcsListRefsResult {
        is_at_repository_root: Some(at_root),
        next_cursor: (total > refs.len()).then_some(refs.len() as u64),
        refs,
        is_repo: true,
        has_primary_remote: remote.is_some(),
        total_count: total as u64,
    }
}

const MAX_BRANCH_COMMITS: usize = 200;

/// One commit of `ReviewBranchCommitsResult.commits`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCommit {
    pub sha: String,
    pub subject: String,
    /// The committer date as git writes it (`%cI`).
    pub committed_at: String,
}

/// `ReviewBranchCommitsResult`: the commits a branch has on top of its base, newest first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BranchCommits {
    pub commits: Vec<BranchCommit>,
    /// Whether there were more than the 200 listed.
    pub truncated: bool,
}

/// `ReviewBranchCommitsResult`: commits on HEAD since `base_ref` (none without a base).
pub async fn branch_commits(cwd: &str, base_ref: Option<&str>) -> BranchCommits {
    let Some(base) = base_ref else { return BranchCommits { commits: vec![], truncated: false } };
    let range = format!("{base}..HEAD");
    let limit = format!("--max-count={}", MAX_BRANCH_COMMITS + 1);
    let out = git(cwd, &["log", &limit, "--format=%H%x1f%s%x1f%cI", &range]).await.unwrap_or_default();
    let mut commits: Vec<BranchCommit> = out
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\u{1f}');
            Some(BranchCommit { sha: it.next()?.to_owned(), subject: it.next()?.to_owned(), committed_at: it.next()?.to_owned() })
        })
        .collect();
    let truncated = commits.len() > MAX_BRANCH_COMMITS;
    commits.truncate(MAX_BRANCH_COMMITS);
    BranchCommits { commits, truncated }
}

async fn version_of(exe: &str, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process::command(exe);
    cmd.args(args);
    let out = crate::process::output(cmd, crate::process::QUICK).await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_owned())
}

/// A value that may be absent, as t3code's `Schema.Option` writes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "_tag")]
pub enum Maybe<T> {
    Some { value: T },
    None,
}

impl<T> From<Option<T>> for Maybe<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => Maybe::Some { value },
            None => Maybe::None,
        }
    }
}

/// Whether a tool was found on the server (`SourceControlDiscoveryStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryStatus {
    Available,
    Missing,
}

/// A tool the server looked for: what is shared by `VcsDiscoveryItem` and
/// `SourceControlProviderDiscoveryItem`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTool {
    pub kind: &'static str,
    pub label: &'static str,
    pub executable: &'static str,
    pub status: DiscoveryStatus,
    /// The first line the tool prints for `--version`.
    pub version: Maybe<String>,
    pub install_hint: &'static str,
    /// Never said by this server.
    pub detail: Maybe<crate::rpc::Never>,
}

impl DiscoveredTool {
    fn found(kind: &'static str, label: &'static str, executable: &'static str, version: Option<String>, install_hint: &'static str) -> Self {
        let status = if version.is_some() { DiscoveryStatus::Available } else { DiscoveryStatus::Missing };
        DiscoveredTool { kind, label, executable, status, version: version.into(), install_hint, detail: Maybe::None }
    }
}

/// A version control system the server looked for (`VcsDiscoveryItem`).
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredVcs {
    #[serde(flatten)]
    pub tool: DiscoveredTool,
    /// Whether this server can work with repositories of the kind.
    pub implemented: bool,
}

/// Whether a provider's CLI is signed in (`SourceControlProviderAuthStatus`);
/// this server does not ask it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderAuthStatus {
    Unknown,
}

/// `SourceControlProviderAuth`, of which this server knows nothing.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAuth {
    pub status: ProviderAuthStatus,
    pub account: Maybe<crate::rpc::Never>,
    pub host: Maybe<crate::rpc::Never>,
    pub detail: Maybe<crate::rpc::Never>,
}

/// A source control provider's CLI the server looked for (`SourceControlProviderDiscoveryItem`).
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredProvider {
    #[serde(flatten)]
    pub tool: DiscoveredTool,
    pub auth: ProviderAuth,
}

/// `SourceControlDiscoveryResult`: the version control systems and provider CLIs found on the server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlDiscovery {
    pub version_control_systems: Vec<DiscoveredVcs>,
    pub source_control_providers: Vec<DiscoveredProvider>,
}

/// `server.discoverSourceControl` result.
pub async fn discover_source_control() -> SourceControlDiscovery {
    let git_v = version_of("git", &["--version"]).await;
    let jj_v = version_of("jj", &["--version"]).await;
    let gh_v = version_of("gh", &["--version"]).await;
    SourceControlDiscovery {
        version_control_systems: vec![
            DiscoveredVcs {
                tool: DiscoveredTool::found("git", "Git", "git", git_v, "Install Git from https://git-scm.com/downloads or with your package manager."),
                implemented: true,
            },
            DiscoveredVcs {
                tool: DiscoveredTool::found("jj", "Jujutsu", "jj", jj_v, "Install Jujutsu with `brew install jj` or from https://github.com/jj-vcs/jj."),
                implemented: false,
            },
        ],
        source_control_providers: vec![DiscoveredProvider {
            tool: DiscoveredTool::found("github", "GitHub", "gh", gh_v, "Install the GitHub CLI from https://cli.github.com."),
            auth: ProviderAuth { status: ProviderAuthStatus::Unknown, account: Maybe::None, host: Maybe::None, detail: Maybe::None },
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::{DiscoveredProvider, DiscoveredTool, DiscoveredVcs, Maybe, ProviderAuth, ProviderAuthStatus, parse_remote};
    use serde_json::json;

    #[test]
    fn discovered_tools_are_written_as_t3code_writes_them() {
        let none = json!({ "_tag": "None" });
        let git = DiscoveredVcs { tool: DiscoveredTool::found("git", "Git", "git", Some("git version 2.50.1".into()), "Install Git."), implemented: true };
        assert_eq!(
            json!(git),
            json!({ "kind": "git", "implemented": true, "label": "Git", "executable": "git", "status": "available",
                "version": { "_tag": "Some", "value": "git version 2.50.1" }, "installHint": "Install Git.", "detail": none })
        );
        let gh = DiscoveredProvider {
            tool: DiscoveredTool::found("github", "GitHub", "gh", None, "Install the GitHub CLI."),
            auth: ProviderAuth { status: ProviderAuthStatus::Unknown, account: Maybe::None, host: Maybe::None, detail: Maybe::None },
        };
        assert_eq!(
            json!(gh),
            json!({ "kind": "github", "label": "GitHub", "executable": "gh", "status": "missing", "version": none, "installHint": "Install the GitHub CLI.",
                "detail": none, "auth": { "status": "unknown", "account": none, "host": none, "detail": none } })
        );
    }

    #[test]
    fn parses_remote_forms() {
        let want = Some(("github.com".into(), "pingdotgg".into(), "t3code".into()));
        assert_eq!(parse_remote("https://github.com/pingdotgg/t3code.git"), want);
        assert_eq!(parse_remote("git@github.com:pingdotgg/t3code.git"), want);
        assert_eq!(parse_remote("ssh://git@github.com/pingdotgg/t3code"), want);
    }
}

/// The branch reviews compare against: the primary remote's default branch,
/// else a local `main` or `master`.
pub async fn default_branch_for(cwd: &str) -> Option<String> {
    let remote = primary_remote(cwd).await.map(|(name, _)| name);
    if let Some(b) = default_branch(cwd, remote.as_deref()).await {
        return Some(match &remote {
            Some(r) => format!("{r}/{b}"),
            None => b,
        });
    }
    for b in ["main", "master"] {
        if git(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{b}")]).await.is_some() {
            return Some(b.to_owned());
        }
    }
    None
}
