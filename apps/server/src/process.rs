//! Child processes, started one way: each is killed when the task waiting
//! for it goes away (a cancelled request, a closed socket, shutdown), and one
//! run to its end is not waited for forever. `just check` refuses a
//! `Command::new` anywhere else.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

/// For work on local files only: a ref, an index, a version string. One
/// still running then is stuck, most often on a prompt nobody will answer.
pub const QUICK: Duration = Duration::from_secs(60);

/// For anything that reaches the network or runs hooks: a fetch, a push,
/// submodules, a commit.
pub const SLOW: Duration = Duration::from_secs(600);

/// A command whose child is killed when what waits for it is dropped.
#[expect(clippy::disallowed_methods, reason = "the one place commands are made")]
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.kill_on_drop(true);
    cmd
}

/// `git` in `cwd`, which fails instead of stopping to ask for a password.
pub fn git(cwd: impl AsRef<Path>) -> tokio::process::Command {
    let mut cmd = command("git");
    cmd.current_dir(cwd).env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Runs `cmd` to its end. A child still running after `limit` is killed and
/// the error says so.
pub async fn output(mut cmd: tokio::process::Command, limit: Duration) -> std::io::Result<Output> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(limit, cmd.output()).await {
        Ok(out) => out,
        Err(_) => Err(gave_up(limit)),
    }
}

/// A command for code that is not async. Nothing kills its child for the
/// caller: run it with `output_blocking`, or own its end (a detached
/// launch, a child the caller kills itself).
#[expect(clippy::disallowed_methods, reason = "the one place commands are made")]
pub fn blocking_command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

/// `output` for code that is not async: runs `cmd` to its end with no input,
/// and kills a child still running after `limit`.
pub fn output_blocking(cmd: &mut std::process::Command, limit: Duration) -> std::io::Result<Output> {
    use std::io::Read;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    // Read as it writes: a child with a full pipe never exits.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(gave_up(limit));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

fn gave_up(limit: Duration) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("it was still running after {} s and was stopped", limit.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_child_that_outlives_its_limit_is_stopped() {
        let started = Instant::now();
        let mut cmd = command("/bin/sleep");
        cmd.arg("30");
        let err = output(cmd, Duration::from_millis(200)).await.expect_err("the limit ends it");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let err = output_blocking(blocking_command("/bin/sleep").arg("30"), Duration::from_millis(200)).expect_err("the limit ends it");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn output_is_whole_even_when_it_fills_a_pipe() {
        // More than a pipe holds (64 KiB): read while the child writes.
        let script = "i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done; echo err >&2";
        let mut cmd = command("/bin/sh");
        cmd.args(["-c", script]);
        let out = output(cmd, QUICK).await.expect("runs");
        assert_eq!(out.stdout.len(), 4000 * 41);
        let out = output_blocking(blocking_command("/bin/sh").args(["-c", script]), QUICK).expect("runs");
        assert_eq!((out.stdout.len(), out.stderr.as_slice()), (4000 * 41, b"err\n".as_slice()));
        assert!(out.status.success());
    }

    #[tokio::test]
    async fn a_child_is_killed_with_the_task_waiting_for_it() {
        let marker = std::env::temp_dir().join(format!("domovoi-process-{}", crate::util::new_uuid()));
        let mut cmd = command("/bin/sh");
        cmd.args(["-c", "sleep 1; touch \"$0\""]).arg(&marker);
        let task = tokio::spawn(async move { output(cmd, QUICK).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "the child did not live to finish");
    }
}
