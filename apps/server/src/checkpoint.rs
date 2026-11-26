//! Workspace checkpoints as hidden Git refs.
//!
//! Capture stages the whole working tree (tracked + untracked, respecting
//! .gitignore) into a temporary index, writes a tree and a commit parented on
//! HEAD, and points a ref under `refs/domovoi/checkpoints/` at it.
//! The user's index, branch and stash are never touched.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("git {command} failed: {stderr}")]
pub struct GitError {
    pub command: String,
    pub stderr: String,
}

async fn git_env(cwd: &str, args: &[&str], env: &[(&str, &str)]) -> Result<String, GitError> {
    let mut cmd = crate::process::command("git");
    cmd.args(args).current_dir(cwd).env("GIT_OPTIONAL_LOCKS", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = crate::process::output(cmd, crate::process::SLOW).await.map_err(|e| GitError { command: args.join(" "), stderr: e.to_string() })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(GitError { command: args.join(" "), stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned() })
    }
}

async fn git(cwd: &str, args: &[&str]) -> Result<String, GitError> {
    git_env(cwd, args, &[]).await
}

/// Ref for the `ordinal`-th checkpoint of a scope.
pub fn checkpoint_ref(scope_id: &str, ordinal: u64) -> String {
    let digest = hex::encode(Sha256::digest(scope_id.as_bytes()));
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..32]);
    format!("refs/domovoi/checkpoints/{key}/ordinal/{ordinal}")
}

pub async fn is_git_repository(cwd: &str) -> bool {
    Path::new(cwd).is_dir() && git(cwd, &["rev-parse", "--is-inside-work-tree"]).await.is_ok_and(|s| s.trim() == "true")
}

/// Captures the working tree into `checkpoint_ref`; returns the commit id.
pub async fn capture(cwd: &str, checkpoint_ref: &str) -> Result<String, GitError> {
    let git_dir = git(cwd, &["rev-parse", "--absolute-git-dir"]).await?.trim().to_owned();
    let tmp_index = format!("{git_dir}/domovoi-checkpoint-{}.index", crate::util::new_uuid());
    let env = [("GIT_INDEX_FILE", tmp_index.as_str())];
    let head = git(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.ok().map(|s| s.trim().to_owned());
    let result = async {
        if head.is_some() {
            git_env(cwd, &["read-tree", "--reset", "HEAD"], &env).await?;
        }
        git_env(cwd, &["add", "-A", "--", "."], &env).await?;
        let tree = git_env(cwd, &["write-tree"], &env).await?.trim().to_owned();
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "domovoi checkpoint"];
        if let Some(h) = &head {
            args.extend(["-p", h.as_str()]);
        }
        let commit = git_env(
            cwd,
            &args,
            &[("GIT_AUTHOR_NAME", "Domovoi"), ("GIT_AUTHOR_EMAIL", "checkpoints@domovoi.invalid"),
              ("GIT_COMMITTER_NAME", "Domovoi"), ("GIT_COMMITTER_EMAIL", "checkpoints@domovoi.invalid")],
        )
        .await?
        .trim()
        .to_owned();
        git(cwd, &["update-ref", checkpoint_ref, &commit]).await?;
        Ok(commit)
    }
    .await;
    let _ = std::fs::remove_file(&tmp_index);
    result
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn has_ref(cwd: &str, checkpoint_ref: &str) -> bool {
    git(cwd, &["rev-parse", "--verify", "--quiet", &format!("{checkpoint_ref}^{{commit}}")]).await.is_ok()
}

/// Restores the working tree to the checkpoint. Files created after it are
/// removed; ignored files are left alone. The index ends up matching HEAD.
pub async fn restore(cwd: &str, checkpoint_ref: &str) -> Result<(), GitError> {
    let commit = git(cwd, &["rev-parse", "--verify", &format!("{checkpoint_ref}^{{commit}}")]).await?.trim().to_owned();
    git(cwd, &["restore", &format!("--source={commit}"), "--staged", "--worktree", "--", "."]).await?;
    // Removing a new .gitignore exposes the files it hid; clean until stable.
    for _ in 0..4 {
        let out = git(cwd, &["clean", "-fd", "--", "."]).await?;
        if !out.lines().any(|l| l.starts_with("Removing ")) {
            break;
        }
    }
    if git(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_ok() {
        git(cwd, &["reset", "--quiet", "--", "."]).await?;
    } else {
        git(cwd, &["rm", "-r", "--cached", "--quiet", "--ignore-unmatch", "--", "."]).await?;
    }
    Ok(())
}

/// Per-file `(path, insertions, deletions)` between two checkpoints.
pub async fn diff_numstat(cwd: &str, from_ref: &str, to_ref: &str) -> Result<Vec<(String, u64, u64)>, GitError> {
    let out = git(cwd, &["diff", "--numstat", "--no-renames", from_ref, to_ref]).await?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let i = it.next()?.parse().unwrap_or(0);
            let d = it.next()?.parse().unwrap_or(0);
            Some((it.next()?.to_owned(), i, d))
        })
        .collect())
}

/// Per-file `(path, kind, additions, deletions)` with kind `added` / `modified` / `deleted`.
pub async fn diff_files(cwd: &str, from_ref: &str, to_ref: &str) -> Result<Vec<(String, String, u64, u64)>, GitError> {
    let status = git(cwd, &["diff", "--name-status", "--no-renames", from_ref, to_ref]).await?;
    let stats = diff_numstat(cwd, from_ref, to_ref).await?;
    Ok(status
        .lines()
        .filter_map(|l| {
            let (code, path) = l.split_once('\t')?;
            let kind = match code.chars().next()? {
                'A' => "added",
                'D' => "deleted",
                _ => "modified",
            };
            let (a, d) = stats.iter().find(|s| s.0 == path).map(|s| (s.1, s.2)).unwrap_or((0, 0));
            Some((path.to_owned(), kind.to_owned(), a, d))
        })
        .collect())
}

pub async fn diff_patch(cwd: &str, from_ref: &str, to_ref: &str) -> Result<String, GitError> {
    git(cwd, &["diff", "--no-renames", "--no-color", from_ref, to_ref]).await
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn delete_refs(cwd: &str, refs: &[String]) -> Result<(), GitError> {
    for r in refs {
        git(cwd, &["update-ref", "-d", r]).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    async fn repo() -> tempdir::Dir {
        let d = tempdir::Dir::new();
        let p = d.path();
        git(p, &["init", "-q", "-b", "main"]).await.unwrap();
        fs::write(format!("{p}/a.txt"), "one\n").unwrap();
        fs::write(format!("{p}/.gitignore"), "ignored.log\n").unwrap();
        git(p, &["add", "-A"]).await.unwrap();
        git_env(p, &["commit", "-qm", "init"], &[("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"), ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t")]).await.unwrap();
        d
    }

    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("domovoi-ckpt-{}", crate::util::new_uuid()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
            pub fn path(&self) -> &str {
                self.0.to_str().unwrap()
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[tokio::test]
    async fn capture_and_restore_round_trip_without_touching_index() {
        let d = repo().await;
        let p = d.path();
        // Turn start: an uncommitted edit and an untracked file already exist.
        fs::write(format!("{p}/a.txt"), "one\ntwo\n").unwrap();
        fs::write(format!("{p}/notes.md"), "draft\n").unwrap();
        let r0 = checkpoint_ref("scope-x", 0);
        capture(p, &r0).await.unwrap();
        assert!(has_ref(p, &r0).await);
        assert_eq!(git(p, &["diff", "--cached", "--name-only"]).await.unwrap(), "", "index untouched");

        // The agent edits, creates and deletes files.
        fs::write(format!("{p}/a.txt"), "changed\n").unwrap();
        fs::write(format!("{p}/new.rs"), "fn main() {}\n").unwrap();
        fs::remove_file(format!("{p}/notes.md")).unwrap();
        fs::write(format!("{p}/ignored.log"), "keep me\n").unwrap();
        let r1 = checkpoint_ref("scope-x", 1);
        capture(p, &r1).await.unwrap();
        let stats = diff_numstat(p, &r0, &r1).await.unwrap();
        let paths: Vec<_> = stats.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "new.rs", "notes.md"]);

        restore(p, &r0).await.unwrap();
        assert_eq!(fs::read_to_string(format!("{p}/a.txt")).unwrap(), "one\ntwo\n");
        assert_eq!(fs::read_to_string(format!("{p}/notes.md")).unwrap(), "draft\n");
        assert!(!Path::new(&format!("{p}/new.rs")).exists(), "post-checkpoint file removed");
        assert!(Path::new(&format!("{p}/ignored.log")).exists(), "ignored files left alone");
        assert_eq!(git(p, &["diff", "--cached", "--name-only"]).await.unwrap(), "", "index matches HEAD");
        assert_eq!(git(p, &["rev-parse", "--abbrev-ref", "HEAD"]).await.unwrap().trim(), "main");
        delete_refs(p, &[r0.clone(), r1]).await.unwrap();
        assert!(!has_ref(p, &r0).await);
    }
}
