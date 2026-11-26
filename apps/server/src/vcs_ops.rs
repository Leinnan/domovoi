//! Git write operations and review diffs:
//! refs, worktrees, init, pull, the merged status snapshot, diff previews and
//! diff file expansion. Failures carry t3code's `GitCommandError` shape.

#![expect(clippy::disallowed_types, reason = "a request field of any JSON type, inside its reader")]

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use domovoi_model::typed::{
    ReviewDiffFileStat, ReviewDiffPreviewResult, ReviewDiffPreviewSource, ReviewDiffPreviewSourceKind, VcsCreateRefResult, VcsPullResult,
    VcsPullStatus, VcsStatus, VcsStatusRemote, VcsSwitchRefResult,
};

/// `<home>/worktrees`, set once at startup.
pub static WORKTREES_DIR: OnceLock<PathBuf> = OnceLock::new();

const REVIEW_DIFF_PATCH_MAX_OUTPUT_BYTES: usize = 120_000;
const REVIEW_DIFF_FILE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const REVIEW_UNTRACKED_MAX_FILES: usize = 5_000;
const REVIEW_DIFF_ARGS: [&str; 10] = [
    "-c",
    "diff.suppressBlankEmpty=false",
    "diff",
    "--find-renames",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--submodule=short",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A git command that did not do its work, as t3code's `GitCommandError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{detail}")]
#[serde(tag = "_tag", rename = "GitCommandError", rename_all = "camelCase")]
pub struct GitCommandError {
    /// The step that ran it, such as `GitVcsDriver.createRef`.
    pub operation: String,
    /// `git` and its subcommand.
    pub command: String,
    pub cwd: String,
    /// What git wrote to stderr, or why it was not run.
    pub detail: String,
    /// How many arguments git was given; said by the git workflow only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_count: Option<usize>,
    /// Absent when git was not run or did not start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

pub fn command_error(operation: &str, command: &str, cwd: &str, detail: impl Into<String>, exit_code: Option<i32>) -> GitCommandError {
    GitCommandError { operation: operation.to_owned(), command: command.to_owned(), cwd: cwd.to_owned(), detail: detail.into(), argument_count: None, exit_code }
}

/// Why `vcs.init` made no repository: t3code's `VcsProcessSpawnError` and `VcsProcessExitError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum InitFailure {
    /// `git init` was not run, or did not start.
    #[error("{cause}")]
    #[serde(rename = "VcsProcessSpawnError")]
    Spawn { operation: &'static str, command: &'static str, cwd: String, cause: String },
    /// `git init` ran and failed.
    #[error("{detail}")]
    #[serde(rename = "VcsProcessExitError", rename_all = "camelCase")]
    Exit { operation: &'static str, command: &'static str, cwd: String, exit_code: i32, detail: String },
}

/// Readers of request fields, as forgiving as the handlers were when they
/// read the payload as JSON: a field that is absent, or not of its type, is
/// as if it had not been sent.
pub mod sent {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    /// The text sent, or the empty text.
    pub fn text<'de, D: Deserializer<'de>>(sent: D) -> Result<String, D::Error> {
        Ok(Value::deserialize(sent)?.as_str().unwrap_or_default().to_owned())
    }

    /// The text sent, if text was sent.
    pub fn text_if_any<'de, D: Deserializer<'de>>(sent: D) -> Result<Option<String>, D::Error> {
        Ok(Value::deserialize(sent)?.as_str().map(str::to_owned))
    }

    /// Whether `true` itself was sent.
    pub fn is_true<'de, D: Deserializer<'de>>(sent: D) -> Result<bool, D::Error> {
        Ok(Value::deserialize(sent)? == true)
    }
}

async fn run(cwd: &str, args: &[&str], env: &[(&str, &str)]) -> std::io::Result<Output> {
    let mut cmd = crate::process::git(cwd);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = crate::process::output(cmd, crate::process::SLOW).await?;
    Ok(Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Runs git; a non-zero exit is a `GitCommandError` whose detail is git's stderr.
pub async fn git_ok(operation: &str, cwd: &str, args: &[&str], fallback: &str) -> Result<Output, GitCommandError> {
    let command = format!("git {}", args.iter().find(|a| !a.starts_with('-') && !a.contains('=')).unwrap_or(&""));
    let out = run(cwd, args, &[]).await.map_err(|e| command_error(operation, &command, cwd, e.to_string(), None))?;
    if out.code != 0 {
        let stderr = out.stderr.trim();
        return Err(command_error(operation, &command, cwd, if stderr.is_empty() { fallback } else { stderr }, Some(out.code)));
    }
    Ok(out)
}

async fn succeeds(cwd: &str, args: &[&str]) -> bool {
    run(cwd, args, &[]).await.is_ok_and(|o| o.code == 0)
}

/// `refuseOptionLikeRefName`: ref names must not be read as options.
fn refuse_option_like(operation: &str, cwd: &str, name: Option<&str>) -> Result<(), GitCommandError> {
    match name {
        Some(n) if n.starts_with('-') => Err(command_error(operation, "git", cwd, format!("Refusing ref name that looks like an option: {n}"), None)),
        _ => Ok(()),
    }
}

pub async fn create_ref(cwd: &str, ref_name: &str, switch: bool) -> Result<VcsCreateRefResult, GitCommandError> {
    refuse_option_like("GitVcsDriver.createRef", cwd, Some(ref_name))?;
    git_ok("GitVcsDriver.createRef", cwd, &["branch", "--", ref_name], "git branch create failed").await?;
    if switch {
        switch_ref(cwd, ref_name).await?;
    }
    Ok(VcsCreateRefResult { ref_name: ref_name.to_owned() })
}

/// `origin/feature` -> `feature`.
fn local_name_from_remote_ref(r: &str) -> Option<String> {
    r.split_once('/').map(|(_, rest)| rest.to_owned()).filter(|s| !s.is_empty())
}

/// `switchRef`: local branches check out directly; a remote branch checks out
/// its tracking branch, creating one when none exists.
pub async fn switch_ref(cwd: &str, ref_name: &str) -> Result<VcsSwitchRefResult, GitCommandError> {
    let op = "GitVcsDriver.switchRef";
    refuse_option_like(op, cwd, Some(ref_name))?;
    let local = succeeds(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{ref_name}")]).await;
    let remote = succeeds(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{ref_name}")]).await;
    let tracking = if remote && !local {
        run(cwd, &["for-each-ref", "--format=%(refname:lstrip=2)\t%(upstream)", "refs/heads"], &[]).await.ok().and_then(|o| {
            let want = format!("refs/remotes/{ref_name}");
            o.stdout.lines().find_map(|l| l.split_once('\t').filter(|(_, up)| *up == want).map(|(b, _)| b.to_owned()))
        })
    } else {
        None
    };
    let candidate = local_name_from_remote_ref(ref_name);
    let candidate_exists = match (&candidate, remote && !local && tracking.is_none()) {
        (Some(c), true) => succeeds(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{c}")]).await,
        _ => false,
    };
    let new_tracking = (!local && remote && tracking.is_none() && !candidate_exists).then_some(candidate).flatten();
    let target: Vec<String> = if local {
        vec!["checkout".into(), ref_name.into()]
    } else if let Some(b) = &new_tracking {
        vec!["checkout".into(), b.clone()]
    } else if remote && tracking.is_none() {
        vec!["checkout".into(), "--track".into(), ref_name.into()]
    } else if let Some(t) = &tracking {
        vec!["checkout".into(), t.clone()]
    } else {
        vec!["checkout".into(), ref_name.into()]
    };
    refuse_option_like(op, cwd, target.last().map(String::as_str))?;
    if let Some(b) = &new_tracking {
        let start = format!("refs/remotes/{ref_name}");
        git_ok(op, cwd, &["branch", "--no-track", "--", b, &start], "git branch create failed").await?;
        if let Err(e) = git_ok(op, cwd, &["branch", &format!("--set-upstream-to={start}"), "--", b], "git branch failed").await {
            let _ = run(cwd, &["update-ref", "-d", &format!("refs/heads/{b}"), &start], &[]).await;
            return Err(e);
        }
    }
    let mut args: Vec<&str> = target.iter().map(String::as_str).collect();
    args.push("--");
    git_ok(op, cwd, &args, "git checkout failed").await?;
    let current = run(cwd, &["branch", "--show-current"], &[]).await.ok().map(|o| o.stdout.trim().to_owned()).filter(|s| !s.is_empty());
    Ok(VcsSwitchRefResult { ref_name: current })
}

async fn git_common_dir(cwd: &str) -> String {
    run(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"], &[]).await.map(|o| o.stdout.trim().to_owned()).unwrap_or_else(|_| cwd.into())
}

/// The default worktree folder: `<worktrees>/<repo>/<branch prefix>-<hash>`.
async fn default_worktree_path(cwd: &str, branch: &str) -> PathBuf {
    let parent = WORKTREES_DIR.get().cloned().unwrap_or_else(|| crate::util::home_dir().join(".domovoi").join("worktrees"));
    let repo = Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let identity = format!("{}\0{branch}", git_common_dir(cwd).await);
    let hash: String = Sha256::digest(identity.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let mut prefix = String::new();
    for c in branch.replace('/', "-").chars() {
        if prefix.len() + c.len_utf8() > 23 {
            break;
        }
        prefix.push(c);
    }
    let base = parent.join(repo).join(format!("{prefix}-{}", &hash[..16]));
    let mut path = base.clone();
    let mut suffix = 2;
    while path.exists() {
        path = PathBuf::from(format!("{}-{suffix}", base.display()));
        suffix += 1;
    }
    path
}

/// A worktree and the branch checked out in it (`VcsWorktree`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub path: String,
    pub ref_name: String,
}

/// `VcsCreateWorktreeResult`: the worktree made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreatedWorktree {
    pub worktree: Worktree,
}

/// `createWorktree`: checks out `ref_name` (or a new branch from it) in a new folder.
pub async fn create_worktree(cwd: &str, ref_name: &str, new_ref_name: Option<&str>, path: Option<&str>) -> Result<CreatedWorktree, GitCommandError> {
    let op = "GitVcsDriver.createWorktree";
    refuse_option_like(op, cwd, Some(ref_name))?;
    refuse_option_like(op, cwd, new_ref_name)?;
    let target_branch = new_ref_name.unwrap_or(ref_name);
    let path = match path {
        Some(p) => PathBuf::from(p),
        None => default_worktree_path(cwd, target_branch).await,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| command_error(op, "git worktree add", cwd, e.to_string(), None))?;
    }
    if let Some(b) = new_ref_name {
        git_ok(op, cwd, &["branch", "--", b, ref_name], "git branch failed").await?;
    }
    let path_s = path.to_string_lossy().into_owned();
    let checkout = new_ref_name.unwrap_or(ref_name);
    if let Err(e) = git_ok(op, cwd, &["worktree", "add", "--", &path_s, checkout], "git worktree add failed").await {
        if let Some(b) = new_ref_name {
            let _ = run(cwd, &["branch", "-D", "--", b], &[]).await;
        }
        return Err(e);
    }
    Ok(CreatedWorktree { worktree: Worktree { path: path_s, ref_name: target_branch.to_owned() } })
}

pub async fn remove_worktree(cwd: &str, path: &str, force: bool) -> Result<(), GitCommandError> {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.extend(["--", path]);
    git_ok("GitVcsDriver.removeWorktree", cwd, &args, "git worktree remove failed").await?;
    Ok(())
}

/// `vcs.init`: `VcsProcessExitError` / `VcsProcessSpawnError` on failure.
pub async fn init(cwd: &str, kind: Option<&str>) -> Result<(), InitFailure> {
    let (operation, command) = ("VcsProvisioning.initRepository", "git init");
    let spawn = |cause: String| InitFailure::Spawn { operation, command, cwd: cwd.to_owned(), cause };
    if kind.is_some_and(|k| k == "jj") {
        return Err(spawn("jj repositories are not supported by this server.".into()));
    }
    match run(cwd, &["init"], &[]).await {
        Ok(o) if o.code == 0 => Ok(()),
        Ok(o) => Err(InitFailure::Exit { operation, command, cwd: cwd.to_owned(), exit_code: o.code, detail: o.stderr.trim().to_owned() }),
        Err(err) => Err(spawn(err.to_string())),
    }
}

/// `pullCurrentBranch`: fast-forward only, from the branch's upstream.
pub async fn pull(cwd: &str) -> Result<VcsPullResult, GitCommandError> {
    let op = "GitVcsDriver.pullCurrentBranch";
    let err = |detail: &str| command_error(op, "git pull", cwd, detail, None);
    let branch = run(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"], &[]).await.ok().filter(|o| o.code == 0).map(|o| o.stdout.trim().to_owned());
    let Some(branch) = branch else { return Err(err("Cannot pull from detached HEAD.")) };
    let upstream = run(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], &[]).await.ok().filter(|o| o.code == 0).map(|o| o.stdout.trim().to_owned());
    let Some(upstream) = upstream else { return Err(err("Current branch has no upstream configured. Push with upstream first.")) };
    let before = run(cwd, &["rev-parse", "HEAD"], &[]).await.map(|o| o.stdout.trim().to_owned()).unwrap_or_default();
    git_ok(op, cwd, &["pull", "--ff-only"], "git pull failed").await?;
    let after = run(cwd, &["rev-parse", "HEAD"], &[]).await.map(|o| o.stdout.trim().to_owned()).unwrap_or_default();
    let status = if before == after { VcsPullStatus::SkippedUpToDate } else { VcsPullStatus::Pulled };
    Ok(VcsPullResult { status, ref_name: branch, upstream_ref: Some(upstream) })
}

/// `vcs.refreshStatus`: the local and remote halves of the status stream,
/// merged into a `VcsStatusResult`. A folder that is no repository has no
/// upstream: it is neither ahead nor behind, and has no pull request.
pub async fn refresh_status(cwd: &str, local_only: bool) -> VcsStatus {
    let local = crate::vcs::local_status(cwd).await;
    let none = || VcsStatusRemote { has_upstream: false, ahead_count: 0, behind_count: 0, ahead_of_default_count: None, pr: None };
    let remote = if local_only { none() } else { crate::vcs::remote_status(cwd).await.unwrap_or_else(none) };
    VcsStatus {
        is_repo: local.is_repo,
        source_control_provider: local.source_control_provider,
        has_primary_remote: local.has_primary_remote,
        is_default_ref: local.is_default_ref,
        ref_name: local.ref_name,
        has_working_tree_changes: local.has_working_tree_changes,
        working_tree: local.working_tree,
        branch_changes: local.branch_changes,
        has_upstream: remote.has_upstream,
        ahead_count: remote.ahead_count,
        behind_count: remote.behind_count,
        ahead_of_default_count: remote.ahead_of_default_count,
        pr: remote.pr,
    }
}

// ---- review ----

/// `parseReviewNumstat` for `--numstat -z` output (renames take two extra fields).
pub fn parse_numstat(stdout: &str) -> Vec<ReviewDiffFileStat> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut files = vec![];
    let mut i = 0;
    while i < fields.len() {
        let f = fields[i];
        let mut parts = f.splitn(3, '\t');
        let (Some(a), Some(d), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
            i += 1;
            continue;
        };
        let num = |s: &str| if s == "-" { Some(0) } else { s.parse::<u64>().ok() };
        let (Some(additions), Some(deletions)) = (num(a), num(d)) else {
            i += 1;
            continue;
        };
        let (path, previous) = if rest.is_empty() {
            let prev = fields.get(i + 1).copied().unwrap_or_default();
            let path = fields.get(i + 2).copied().unwrap_or_default();
            i += 2;
            (path, Some(prev))
        } else {
            (rest, None)
        };
        files.push(ReviewDiffFileStat { path: path.to_owned(), previous_path: previous.map(str::to_owned), additions: Number::from(additions), deletions: Number::from(deletions) });
        i += 1;
    }
    files
}

/// The hash a client compares to tell whether a diff changed: of the JSON
/// text of `[diff, files]`.
fn diff_hash(diff: &str, files: &[ReviewDiffFileStat]) -> String {
    let hashed = serde_json::to_string(&(diff, files)).unwrap_or_default();
    Sha256::digest(hashed.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn truncate_output(mut s: String, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated]\n");
    (s, true)
}

/// A temporary index where untracked files are intent-to-add, so `git diff`
/// shows them as new files.
struct ReviewIndex {
    path: PathBuf,
}

impl Drop for ReviewIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn review_index(cwd: &str, path_args: &[String], only: Option<&str>) -> Option<ReviewIndex> {
    let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
    args.extend(path_args.iter().map(String::as_str));
    let out = run(cwd, &args, &[]).await.ok().filter(|o| o.code == 0)?;
    let paths: Vec<&str> = out.stdout.split('\0').filter(|p| !p.is_empty()).filter(|p| only.is_none_or(|o| *p == o || *p == format!("{o}/"))).collect();
    if paths.is_empty() || paths.len() > REVIEW_UNTRACKED_MAX_FILES {
        return None;
    }
    let git_index = run(cwd, &["rev-parse", "--path-format=absolute", "--git-path", "index"], &[]).await.ok()?.stdout.trim().to_owned();
    let tmp = std::env::temp_dir().join(format!("domovoi-review-index-{}", crate::util::new_uuid()));
    if std::fs::copy(&git_index, &tmp).is_err() {
        // An unborn repository has no index yet; start from an empty one.
        std::fs::write(&tmp, b"").ok()?;
        let _ = std::fs::remove_file(&tmp);
    }
    let tmp_s = tmp.to_string_lossy().into_owned();
    let mut add = vec!["add", "--intent-to-add", "--force", "--"];
    add.extend(paths.iter().copied());
    run(cwd, &add, &[("GIT_INDEX_FILE", &tmp_s)]).await.ok().filter(|o| o.code == 0)?;
    Some(ReviewIndex { path: tmp })
}

async fn empty_tree(cwd: &str) -> String {
    run(cwd, &["hash-object", "-t", "tree", "/dev/null"], &[]).await.map(|o| o.stdout.trim().to_owned()).unwrap_or_else(|_| "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into())
}

/// The merge base with `explicit` (or the repository's default branch).
async fn merge_base(cwd: &str, explicit: Option<&str>) -> (Option<String>, String) {
    let base = match explicit {
        Some(b) => Some(b.to_owned()),
        None => crate::vcs::default_branch_for(cwd).await,
    };
    let Some(base) = base else { return (None, "HEAD".into()) };
    let comparison = if !base.starts_with("refs/") && succeeds(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{base}")]).await {
        format!("refs/heads/{base}")
    } else {
        base.clone()
    };
    match run(cwd, &["merge-base", "--end-of-options", &comparison, "HEAD"], &[]).await {
        Ok(o) if o.code == 0 && !o.stdout.trim().is_empty() => (Some(base), o.stdout.trim().to_owned()),
        _ => (Some(base), "HEAD".into()),
    }
}

async fn tracked_diff(cwd: &str, reference: &str, path_args: &[String], extra: &[&str], index: Option<&ReviewIndex>, patch_limit: usize, metadata_only: bool) -> (String, bool, Vec<ReviewDiffFileStat>) {
    let env_owned = index.map(|i| i.path.to_string_lossy().into_owned());
    let env: Vec<(&str, &str)> = env_owned.as_deref().map(|p| vec![("GIT_INDEX_FILE", p)]).unwrap_or_default();
    let mut base: Vec<&str> = REVIEW_DIFF_ARGS.to_vec();
    base.extend(extra);
    let stat_args = |r: &str| -> Vec<String> {
        let mut a: Vec<String> = base.iter().map(|s| s.to_string()).collect();
        a.extend(["--numstat".into(), "-z".into(), r.to_owned(), "--".into()]);
        a.extend(path_args.iter().cloned());
        a
    };
    let mut reference = reference.to_owned();
    let args = stat_args(&reference);
    let mut stat = run(cwd, &args.iter().map(String::as_str).collect::<Vec<_>>(), &env).await.ok();
    if stat.as_ref().is_some_and(|o| o.code != 0) && reference == "HEAD" {
        // Unborn HEAD: compare against the empty tree.
        reference = empty_tree(cwd).await;
        let args = stat_args(&reference);
        stat = run(cwd, &args.iter().map(String::as_str).collect::<Vec<_>>(), &env).await.ok();
    }
    let files = stat.filter(|o| o.code == 0).map(|o| parse_numstat(&o.stdout)).unwrap_or_default();
    if metadata_only || files.is_empty() {
        return (String::new(), false, files);
    }
    let mut patch_args = base;
    patch_args.extend(["--patch", &reference, "--"]);
    patch_args.extend(path_args.iter().map(String::as_str));
    let patch = run(cwd, &patch_args, &env).await.map(|o| o.stdout).unwrap_or_default();
    let (diff, truncated) = truncate_output(patch, patch_limit);
    (diff, truncated, files)
}

/// The one file a diff preview is asked for (`ReviewDiffPreviewInput.file`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreviewFile {
    #[serde(default, deserialize_with = "sent::text")]
    pub path: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub previous_path: Option<String>,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub source_kind: Option<String>,
}

/// A `file` that is not an object is as if none had been sent.
fn file_if_any<'de, D: serde::Deserializer<'de>>(sent: D) -> Result<Option<DiffPreviewFile>, D::Error> {
    let sent = Value::deserialize(sent)?;
    if !sent.is_object() {
        return Ok(None);
    }
    DiffPreviewFile::deserialize(sent).map(Some).map_err(serde::de::Error::custom)
}

/// `ReviewDiffPreviewInput`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPreviewInput {
    #[serde(default, deserialize_with = "sent::text")]
    pub cwd: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub base_ref: Option<String>,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub commit_sha: Option<String>,
    #[serde(default, deserialize_with = "sent::is_true")]
    pub ignore_whitespace: bool,
    /// The one kind of source wanted.
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub source_kind: Option<String>,
    #[serde(default, deserialize_with = "sent::is_true")]
    pub tracked_only: bool,
    #[serde(default, deserialize_with = "sent::is_true")]
    pub metadata_only: bool,
    #[serde(default, deserialize_with = "file_if_any")]
    pub file: Option<DiffPreviewFile>,
}

/// `review.getDiffPreview`: uncommitted changes and the branch range (or one commit).
pub async fn diff_preview(input: &DiffPreviewInput) -> Result<ReviewDiffPreviewResult, GitCommandError> {
    let cwd = input.cwd.as_str();
    let preview = |sources: Vec<ReviewDiffPreviewSource>, incomplete: Option<bool>| ReviewDiffPreviewResult {
        cwd: cwd.to_owned(),
        generated_at: crate::time_fmt::iso_now().into(),
        sources,
        incomplete,
    };
    let Some(root) = run(cwd, &["rev-parse", "--show-toplevel"], &[]).await.ok().filter(|o| o.code == 0).map(|o| o.stdout.trim().to_owned()) else {
        return Ok(preview(vec![], None));
    };
    let file = input.file.as_ref();
    let path_args: Vec<String> = file
        .map(|f| {
            let mut v = vec![format!(":(top,literal){}", f.path)];
            if let Some(p) = &f.previous_path {
                v.push(format!(":(top,literal){p}"));
            }
            v
        })
        .unwrap_or_default();
    let patch_limit = if file.is_some() { REVIEW_DIFF_FILE_MAX_OUTPUT_BYTES } else { REVIEW_DIFF_PATCH_MAX_OUTPUT_BYTES };
    let ignore_ws: Vec<&str> = if input.ignore_whitespace { vec!["--ignore-all-space"] } else { vec![] };
    let metadata_only = input.metadata_only && file.is_none();
    let patches_deferred = metadata_only.then_some(true);
    let commit_err = |d: &str| command_error("GitVcsDriver.getReviewDiffPreview.commit", "git", &root, d, None);

    if let Some(sha) = input.commit_sha.as_deref().or_else(|| file.filter(|f| f.source_kind.as_deref() == Some("commit")).and(Some(""))) {
        if sha.is_empty() {
            return Err(commit_err("Selected commit request is invalid."));
        }
        let resolved = git_ok("GitVcsDriver.getReviewDiffPreview.commit", &root, &["rev-parse", "--verify", "--end-of-options", &format!("{sha}^{{commit}}")], "Selected commit is no longer available.").await?;
        let sha = resolved.stdout.trim().to_owned();
        let parent = match run(&root, &["rev-parse", "--verify", "--quiet", &format!("{sha}^")], &[]).await {
            Ok(o) if o.code == 0 => o.stdout.trim().to_owned(),
            _ => empty_tree(&root).await,
        };
        let subject = run(&root, &["log", "-1", "--format=%s", &sha], &[]).await.map(|o| o.stdout.trim().to_owned()).unwrap_or_default();
        let mut args: Vec<&str> = REVIEW_DIFF_ARGS.to_vec();
        args.extend(&ignore_ws);
        let mut stat = args.clone();
        stat.extend(["--numstat", "-z", &parent, &sha, "--"]);
        stat.extend(path_args.iter().map(String::as_str));
        let files = parse_numstat(&git_ok("GitVcsDriver.getReviewDiffPreview.commitStat", &root, &stat, "git diff failed").await?.stdout);
        let (diff, truncated) = if metadata_only {
            (String::new(), false)
        } else {
            let mut p = args.clone();
            p.extend(["--patch", &parent, &sha, "--"]);
            p.extend(path_args.iter().map(String::as_str));
            truncate_output(git_ok("GitVcsDriver.getReviewDiffPreview.commitPatch", &root, &p, "git diff failed").await?.stdout, patch_limit)
        };
        let source = ReviewDiffPreviewSource {
            id: sha.clone(),
            kind: ReviewDiffPreviewSourceKind::Commit,
            title: format!("Commit {}", &sha[..7.min(sha.len())]),
            commit_subject: Some(subject).filter(|s| !s.is_empty()),
            base_ref: Some(parent),
            head_ref: Some(sha),
            diff_hash: diff_hash(&diff, &files),
            diff,
            truncated,
            patches_deferred,
            files: Some(files),
        };
        return Ok(preview(vec![source], None));
    }

    let source_kind = file.and_then(|f| f.source_kind.as_deref()).or(input.source_kind.as_deref());
    let index = if input.tracked_only { None } else { review_index(&root, &path_args, file.map(|f| f.path.as_str())).await };
    let (base_ref, mb) = if source_kind == Some("working-tree") {
        (input.base_ref.clone(), "HEAD".to_owned())
    } else {
        merge_base(&root, input.base_ref.as_deref()).await
    };
    let mut extra = ignore_ws.clone();
    extra.push("--minimal");
    let dirty = tracked_diff(&root, "HEAD", &path_args, &extra, index.as_ref(), patch_limit, metadata_only).await;
    let base = if mb == "HEAD" { dirty.clone() } else { tracked_diff(&root, &mb, &path_args, &extra, index.as_ref(), patch_limit, metadata_only).await };
    let branch = run(&root, &["branch", "--show-current"], &[]).await.ok().map(|o| o.stdout.trim().to_owned()).filter(|s| !s.is_empty());
    let source = |kind: ReviewDiffPreviewSourceKind, title: String, base_ref: Option<String>, head_ref: Option<String>, (diff, truncated, files): (String, bool, Vec<ReviewDiffFileStat>)| {
        ReviewDiffPreviewSource {
            id: kind.as_str().to_owned(),
            kind,
            title,
            commit_subject: None,
            base_ref,
            head_ref,
            diff_hash: diff_hash(&diff, &files),
            diff,
            truncated,
            patches_deferred,
            files: Some(files),
        }
    };
    let mut sources = vec![
        source(ReviewDiffPreviewSourceKind::WorkingTree, "Uncommitted".into(), Some("HEAD".into()), None, dirty),
        source(
            ReviewDiffPreviewSourceKind::BranchRange,
            base_ref.as_ref().map(|b| format!("Changes vs {b}")).unwrap_or_else(|| "Changes".into()),
            base_ref,
            Some(branch.unwrap_or_else(|| "HEAD".into())),
            base,
        ),
    ];
    if let Some(k) = input.source_kind.as_deref() {
        sources.retain(|s| s.kind.as_str() == k);
    }
    Ok(preview(sources, input.tracked_only.then_some(true)))
}

/// `ReviewDiffFileContentsInput`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileContentsInput {
    #[serde(default, deserialize_with = "sent::text")]
    pub cwd: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub source_kind: Option<String>,
    /// `change` when not said.
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub change_type: Option<String>,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub base_ref: Option<String>,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    pub head_ref: Option<String>,
    #[serde(default, deserialize_with = "sent::text")]
    pub old_path: String,
    #[serde(default, deserialize_with = "sent::text")]
    pub new_path: String,
}

/// `ReviewDiffFileContentsResult`: one file before and after a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

/// `review.getDiffFileContents`: both sides of one file, for expanding context.
pub async fn diff_file_contents(input: &DiffFileContentsInput) -> Result<DiffFileContents, GitCommandError> {
    let cwd = input.cwd.as_str();
    let err = |d: String| command_error("GitVcsDriver.getReviewDiffFileContents", "git", cwd, d, None);
    let root = git_ok("GitVcsDriver.getReviewDiffFileContents.repositoryRoot", cwd, &["rev-parse", "--show-toplevel"], "Could not resolve the Git repository root.").await?.stdout.trim().to_owned();
    let change = input.change_type.as_deref().unwrap_or("change");
    let source_kind = input.source_kind.as_deref();
    let old_path = input.old_path.as_str();
    let new_path = input.new_path.as_str();
    let at_revision = |rev: String, path: String| async move {
        let out = git_ok("GitVcsDriver.getReviewDiffFileContents.revision", cwd, &["show", "--end-of-options", &format!("{rev}:{path}")], "git show failed").await?;
        if out.stdout.len() > REVIEW_DIFF_FILE_MAX_OUTPUT_BYTES {
            return Err(command_error("GitVcsDriver.getReviewDiffFileContents", "git", cwd, format!("Diff file '{path}' exceeds the 1 MB expansion limit."), None));
        }
        if out.stdout.contains('\0') {
            return Err(command_error("GitVcsDriver.getReviewDiffFileContents", "git", cwd, format!("Cannot expand binary file '{path}'."), None));
        }
        Ok(out.stdout)
    };
    if source_kind == Some("commit") {
        let head = input.head_ref.as_deref().ok_or_else(|| err("Selected commit is no longer available.".into()))?;
        let parent = run(cwd, &["rev-parse", "--verify", "--quiet", &format!("{head}^")], &[]).await.ok().filter(|o| o.code == 0).map(|o| o.stdout.trim().to_owned());
        if input.base_ref.as_deref() != parent.as_deref() {
            return Err(err("Selected commit changed. Refresh the comparison.".into()));
        }
        let old_contents = if change == "new" { String::new() } else { at_revision(parent.unwrap_or_default(), old_path.to_owned()).await? };
        let new_contents = if change == "deleted" { String::new() } else { at_revision(head.to_owned(), new_path.to_owned()).await? };
        return Ok(DiffFileContents { old_contents, new_contents });
    }
    let old_rev = if source_kind == Some("working-tree") {
        input.base_ref.as_deref().unwrap_or("HEAD").to_owned()
    } else {
        merge_base(cwd, input.base_ref.as_deref()).await.1
    };
    let old_contents = if change == "new" { String::new() } else { at_revision(old_rev, old_path.to_owned()).await? };
    let new_contents = if change == "deleted" {
        String::new()
    } else {
        let real_root = std::fs::canonicalize(&root).map_err(|e| err(e.to_string()))?;
        let target = std::fs::canonicalize(Path::new(&root).join(new_path)).map_err(|_| err(format!("Could not resolve diff file '{new_path}'.")))?;
        if !target.starts_with(&real_root) {
            return Err(err(format!("Diff file '{new_path}' resolves outside the review workspace.")));
        }
        let meta = std::fs::metadata(&target).map_err(|_| err(format!("Could not inspect diff file '{new_path}'.")))?;
        if !meta.is_file() {
            return Err(err(format!("Diff path '{new_path}' is not a file.")));
        }
        if meta.len() as usize > REVIEW_DIFF_FILE_MAX_OUTPUT_BYTES {
            return Err(err(format!("Diff file '{new_path}' exceeds the 1 MB expansion limit.")));
        }
        let bytes = std::fs::read(&target).map_err(|_| err(format!("Could not read diff file '{new_path}'.")))?;
        if bytes.contains(&0) {
            return Err(err(format!("Cannot expand binary file '{new_path}'.")));
        }
        String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes)).into_owned()
    };
    Ok(DiffFileContents { old_contents, new_contents })
}

// ---- temporary worktree branches ----

/// The branch a new worktree starts on, until it is named: `domovoi/<8 hex>`.
pub fn temporary_branch_name() -> String {
    format!("domovoi/{}", &crate::util::new_uuid().replace('-', "")[..8])
}

/// A branch `a/b` cannot exist when a branch `a` does: flatten it to `a-b`.
pub async fn usable_branch_name(cwd: &str, branch: &str) -> String {
    let parts: Vec<&str> = branch.split('/').collect();
    for depth in 1..parts.len() {
        if succeeds(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{}", parts[..depth].join("/"))]).await {
            return branch.replace('/', "-");
        }
    }
    branch.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn preview_input(sent: Value) -> DiffPreviewInput {
        serde_json::from_value(sent).unwrap()
    }

    fn contents_input(sent: Value) -> DiffFileContentsInput {
        serde_json::from_value(sent).unwrap()
    }

    #[test]
    fn failures_are_written_as_tagged_errors() {
        assert_eq!(
            json!(command_error("GitVcsDriver.createRef", "git branch", "/repo", "fatal: a branch named 'x' already exists", Some(128))),
            json!({ "_tag": "GitCommandError", "operation": "GitVcsDriver.createRef", "command": "git branch", "cwd": "/repo",
                "detail": "fatal: a branch named 'x' already exists", "exitCode": 128 })
        );
        assert_eq!(
            json!(command_error("GitVcsDriver.pullCurrentBranch", "git pull", "/repo", "Cannot pull from detached HEAD.", None)),
            json!({ "_tag": "GitCommandError", "operation": "GitVcsDriver.pullCurrentBranch", "command": "git pull", "cwd": "/repo", "detail": "Cannot pull from detached HEAD." })
        );
        assert_eq!(
            json!(GitCommandError { argument_count: Some(4), ..command_error("GitVcsDriver.commit", "git commit", "/repo", "nothing to commit", Some(1)) }),
            json!({ "_tag": "GitCommandError", "operation": "GitVcsDriver.commit", "command": "git commit", "cwd": "/repo", "detail": "nothing to commit",
                "argumentCount": 4, "exitCode": 1 })
        );
        let (operation, command) = ("VcsProvisioning.initRepository", "git init");
        assert_eq!(
            json!(InitFailure::Spawn { operation, command, cwd: "/gone".into(), cause: "No such file or directory (os error 2)".into() }),
            json!({ "_tag": "VcsProcessSpawnError", "operation": "VcsProvisioning.initRepository", "command": "git init", "cwd": "/gone",
                "cause": "No such file or directory (os error 2)" })
        );
        assert_eq!(
            json!(InitFailure::Exit { operation, command, cwd: "/ro".into(), exit_code: 1, detail: "/ro/.git: Permission denied".into() }),
            json!({ "_tag": "VcsProcessExitError", "operation": "VcsProvisioning.initRepository", "command": "git init", "cwd": "/ro",
                "exitCode": 1, "detail": "/ro/.git: Permission denied" })
        );
    }

    #[test]
    fn request_fields_not_of_their_type_are_as_if_not_sent() {
        let input = preview_input(json!({ "cwd": 7, "baseRef": null, "commitSha": ["x"], "ignoreWhitespace": "yes", "trackedOnly": true, "file": "a.txt" }));
        assert_eq!((input.cwd.as_str(), input.base_ref, input.commit_sha, input.ignore_whitespace, input.tracked_only, input.metadata_only), ("", None, None, false, true, false));
        assert!(input.file.is_none());
        let input = preview_input(json!({ "cwd": "/repo", "file": { "path": "a.txt", "previousPath": null } }));
        let file = input.file.unwrap();
        assert_eq!((file.path.as_str(), file.previous_path, file.source_kind), ("a.txt", None, None));
        let input = contents_input(json!({ "cwd": "/repo", "changeType": 3 }));
        assert_eq!((input.change_type, input.old_path.as_str(), input.source_kind), (None, "", None));
    }

    #[tokio::test]
    async fn a_status_outside_a_repository_is_whole_and_has_no_upstream() {
        let dir = std::env::temp_dir().join(format!("domovoi-ops-plain-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let whole = json!({ "isRepo": false, "hasPrimaryRemote": false, "isDefaultRef": false, "refName": null, "hasWorkingTreeChanges": false,
            "workingTree": { "files": [], "insertions": 0, "deletions": 0 }, "hasUpstream": false, "aheadCount": 0, "behindCount": 0, "pr": null });
        assert_eq!(json!(refresh_status(&cwd, false).await), whole);
        assert_eq!(json!(refresh_status(&cwd, true).await), whole);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repo() -> String {
        let d = std::env::temp_dir().join(format!("domovoi-ops-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        let g = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&d).output().unwrap().status.success(), "{args:?}");
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "one\ntwo\n").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]);
        d.canonicalize().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn numstat_parses_renames() {
        let out = "3\t1\tsrc/a.rs\0-\t-\t\0old.bin\0new.bin\0";
        assert_eq!(
            json!(parse_numstat(out)),
            json!([
                { "path": "src/a.rs", "previousPath": null, "additions": 3, "deletions": 1 },
                { "path": "new.bin", "previousPath": "old.bin", "additions": 0, "deletions": 0 },
            ])
        );
        // The hash is of the JSON text of the diff and its files, fields in this order.
        let hashed = r#"["d",[{"path":"src/a.rs","previousPath":null,"additions":3,"deletions":1},{"path":"new.bin","previousPath":"old.bin","additions":0,"deletions":0}]]"#;
        let expected: String = Sha256::digest(hashed.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(diff_hash("d", &parse_numstat(out)), expected);
    }

    #[tokio::test]
    async fn refs_and_worktrees() {
        let r = repo();
        let dir = std::env::temp_dir().join(format!("domovoi-wt-{}", crate::util::new_uuid()));
        let _ = WORKTREES_DIR.set(dir.clone());
        assert_eq!(json!(create_ref(&r, "feature", true).await.unwrap()), json!({ "refName": "feature" }));
        assert_eq!(json!(switch_ref(&r, "main").await.unwrap()), json!({ "refName": "main" }));
        assert_eq!(create_ref(&r, "-x", false).await.unwrap_err().exit_code, None, "refused before git ran");
        let e = create_ref(&r, "feature", false).await.unwrap_err();
        assert!(e.detail.contains("already exists"), "{e}");
        let wt = create_worktree(&r, "main", Some("t3/abcd1234"), None).await.unwrap();
        assert_eq!(json!(wt), json!({ "worktree": { "path": wt.worktree.path, "refName": "t3/abcd1234" } }));
        let path = wt.worktree.path;
        assert!(Path::new(&path).join("a.txt").is_file() && path.contains("t3-abcd1234-"), "{path}");
        assert_eq!(usable_branch_name(&r, "other/x").await, "other/x");
        assert_eq!(usable_branch_name(&r, "feature/x").await, "feature-x", "a branch `feature` exists");
        remove_worktree(&r, &path, true).await.unwrap();
        assert!(!Path::new(&path).exists());
        let e = pull(&r).await.unwrap_err();
        assert!(e.detail.contains("no upstream"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn diff_preview_includes_untracked_files_and_expands_contents() {
        let r = repo();
        std::fs::write(Path::new(&r).join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(Path::new(&r).join("new.txt"), "fresh\n").unwrap();
        let p = diff_preview(&preview_input(json!({ "cwd": r, "sourceKind": "working-tree" }))).await.unwrap();
        let s = &p.sources[0];
        assert_eq!(s.kind, ReviewDiffPreviewSourceKind::WorkingTree);
        let paths: Vec<&str> = s.files.iter().flatten().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "new.txt"], "{s:?}");
        assert!(s.diff.contains("+++ b/new.txt"));
        let c = diff_file_contents(&contents_input(json!({ "cwd": r, "sourceKind": "working-tree", "changeType": "change", "baseRef": null, "headRef": null, "oldPath": "a.txt", "newPath": "a.txt" }))).await.unwrap();
        assert_eq!(json!(c), json!({ "oldContents": "one\ntwo\n", "newContents": "one\nTWO\n" }));
        let head = run(&r, &["rev-parse", "HEAD"], &[]).await.unwrap().stdout.trim().to_owned();
        let p = diff_preview(&preview_input(json!({ "cwd": r, "commitSha": head }))).await.unwrap();
        assert_eq!((&p.sources[0].kind, p.sources[0].commit_subject.as_deref()), (&ReviewDiffPreviewSourceKind::Commit, Some("first")));
        let outside = diff_file_contents(&contents_input(json!({ "cwd": r, "sourceKind": "working-tree", "changeType": "change", "baseRef": null, "headRef": null, "oldPath": "a.txt", "newPath": "../x" }))).await;
        assert!(outside.is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}

/// Why a project's checkout isn't pulled automatically, if it isn't:
/// not a repository, not on its default
/// branch, no upstream, local changes, or local commits.
pub async fn auto_pull_blocker(cwd: &str) -> Option<&'static str> {
    let status = crate::vcs::local_status(cwd).await;
    if !status.is_repo {
        return Some("not-a-repository");
    }
    if !status.is_default_ref {
        return Some("not-on-default-branch");
    }
    if !run(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], &[]).await.is_ok_and(|o| o.code == 0) {
        return Some("no-upstream");
    }
    if status.has_working_tree_changes {
        return Some("working-tree-changes");
    }
    let ahead = run(cwd, &["rev-list", "--count", "@{u}..HEAD"], &[]).await.ok().and_then(|o| o.stdout.trim().parse::<u64>().ok()).unwrap_or(1);
    (ahead > 0).then_some("local-commits")
}

/// Fast-forwards each project marked for it (`autoPull`, or the
/// `defaultAutoPull` / `projectAutoPullOverrides` settings) at startup.
pub async fn auto_pull_projects(projects: &[crate::wire::shell::ProjectShell], settings: &domovoi_model::typed::ServerSettings) {
    let overrides = settings.project_auto_pull_overrides.as_ref();
    let mut roots: Vec<&str> = projects.iter().filter(|p| p.archived_at.is_none()).filter(|p| {
        overrides.and_then(|overrides| overrides.get(p.id.as_str()).copied())
            .unwrap_or(p.auto_pull || settings.default_auto_pull == Some(true))
    }).map(|p| p.workspace_root.as_str()).collect();
    roots.sort_unstable();
    roots.dedup();
    for cwd in roots {
        match auto_pull_blocker(cwd).await {
            Some(reason) => tracing::debug!(%cwd, reason, "skipped automatic project pull"),
            None => match pull(cwd).await {
                Ok(r) => tracing::info!(%cwd, status = r.status.as_str(), "automatic project pull"),
                Err(e) => tracing::warn!(%cwd, "automatic project pull failed: {}", e.detail),
            },
        }
    }
}

