//! Child processes, started one way: each is killed when the task waiting
//! for it goes away (a cancelled request, a closed socket, shutdown), and one
//! run to its end is not waited for forever. `just check` refuses a
//! `Command::new` anywhere else.
//!
//! On Windows a child opens no console window. Killing a child doesn't end
//! what it started, and npm's `claude.cmd` is `cmd.exe` running `node`, so a
//! long-lived child is held in a [`Tree`]: its whole process tree ends with
//! it, and with this server.

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

/// `CREATE_NO_WINDOW`: a console program started by the server (which may
/// have no console itself) opens no window of its own.
#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000;

/// Lets [`interrupt`] ask `cmd`'s child to stop, as SIGTERM does on Unix:
/// it starts as a process group of its own, on this server's console
/// (hidden when the server's is), where a Ctrl+Break can reach it. Without a
/// console here there is nothing to share, so it stays as [`command`] made
/// it (`false`): then only ending its tree stops it.
#[cfg(windows)]
pub fn interruptible(cmd: &mut tokio::process::Command) -> bool {
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
    // SAFETY: plain call; 0 means this process has no console.
    if unsafe { windows_sys::Win32::System::Console::GetConsoleCP() } == 0 {
        return false;
    }
    // In place of `NO_WINDOW`: a child on this console opens no window either.
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    true
}

/// Sends Ctrl+Break to the process group `pid` leads: only a child started
/// [`interruptible`], since for any other id Windows sends it to every
/// process on this console, this server too. Whether it was sent.
#[cfg(windows)]
pub fn interrupt(pid: u32) -> bool {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: plain call, for a group this server made.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
}

/// A command whose child is killed when what waits for it is dropped.
///
/// On Windows a batch-file shim (npm's `claude.cmd`, `codex.cmd`) is started
/// as what it runs: its interpreter and script, with no `cmd.exe` between.
/// `cmd.exe` would read the arguments (a prompt, a JSON setting) as its own
/// syntax, and std refuses to pass the ones it can't escape safely.
#[expect(clippy::disallowed_methods, reason = "the one place commands are made")]
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[cfg(windows)]
    let program = windows_program(program.as_ref());
    #[cfg(windows)]
    let mut cmd = match shim_target(Path::new(&program)) {
        Some(Shim { program, script }) => {
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(script);
            cmd
        }
        None => tokio::process::Command::new(program),
    };
    #[cfg(not(windows))]
    let mut cmd = tokio::process::Command::new(program);
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    cmd
}

/// `program` as a shell would find it on Windows: one named without an
/// extension (`npx`, an ACP agent's `commandPath`) gets the first of
/// `PATHEXT`'s that exists. Windows itself only tries `.exe`, so `npx.cmd`
/// would not be found. Names are looked up once and remembered.
#[cfg(windows)]
fn windows_program(program: &OsStr) -> std::ffi::OsString {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static FOUND: LazyLock<Mutex<HashMap<std::ffi::OsString, std::path::PathBuf>>> = LazyLock::new(Mutex::default);
    let path = Path::new(program);
    if path.extension().is_some() {
        return program.to_owned();
    }
    if path.components().count() > 1 || path.is_absolute() {
        return crate::providers::runnable(path).map_or_else(|| program.to_owned(), std::path::PathBuf::into_os_string);
    }
    if let Some(found) = crate::util::locked(&FOUND).get(program) {
        return found.clone().into_os_string();
    }
    match crate::providers::find_in(&crate::providers::search_dirs(), &program.to_string_lossy()) {
        Some(found) => {
            crate::util::locked(&FOUND).insert(program.to_owned(), found.clone());
            found.into_os_string()
        }
        None => program.to_owned(),
    }
}

/// What a batch-file shim runs: a program, and the script it is given first
/// (none when the shim starts an executable of its own).
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub struct Shim {
    pub program: std::path::PathBuf,
    pub script: Option<std::path::PathBuf>,
}

/// What a batch file runs, when it is only a shim:
/// - npm's: `"%~dp0\node_modules\…\cli.js" %*` (older npm) or cmd-shim's
///   `"%dp0%\node_modules\…\cli.js" %*`, run with the `node.exe` beside it
///   or on PATH; or one that starts a packaged executable directly
///   (`"%dp0%\node_modules\opencode-ai\bin\opencode.exe" %*`);
/// - a one-line wrapper: `@python "%~dp0tool" %*`, `node "%~dp0launcher.js" %*`
///   (after an `@ECHO off`).
///
/// `None` for anything else, which runs as a batch file.
#[cfg(any(windows, test))]
pub fn shim_target(program: &Path) -> Option<Shim> {
    let ext = program.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let text = std::fs::read_to_string(program).ok().filter(|t| t.len() <= 16 * 1024)?;
    let dir = program.parent()?;
    let find = |name: &str| -> Option<std::path::PathBuf> {
        let beside = dir.join(name);
        if beside.extension().is_some() && beside.is_file() {
            return Some(beside);
        }
        crate::providers::find_in(&crate::providers::search_dirs(), name)
    };
    // npm: a script under node_modules, after the shim's own folder.
    for marker in ["\"%~dp0\\", "\"%dp0%\\", "\"%~dp0"] {
        let mut rest = text.as_str();
        while let Some(at) = rest.find(marker) {
            let after = &rest[at + marker.len()..];
            let Some(end) = after.find('"') else { break };
            let rel = after[..end].trim_start_matches('\\');
            let lower = rel.to_ascii_lowercase();
            let target = dir.join(rel);
            if lower.starts_with("node_modules") && target.is_file() {
                if [".js", ".mjs", ".cjs"].iter().any(|e| lower.ends_with(e)) {
                    return Some(Shim { program: find("node.exe").or_else(|| find("node"))?, script: Some(target) });
                }
                if lower.ends_with(".exe") {
                    return Some(Shim { program: target, script: None });
                }
            }
            rest = &after[end..];
        }
    }
    // A one-line wrapper: `[@]interpreter "%~dp0script" %*`.
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.to_ascii_lowercase().starts_with("@echo off")).collect();
    if let [line] = lines.as_slice() {
        let line = line.trim_start_matches('@');
        let (interpreter, rest) = line.split_once(' ')?;
        let rest = rest.trim();
        let script = rest.strip_prefix("\"%~dp0")?.strip_suffix("\" %*")?.trim_start_matches('\\');
        let script = dir.join(script);
        if script.is_file() && !interpreter.contains('%') {
            return Some(Shim { program: find(interpreter.trim_matches('"'))?, script: Some(script) });
        }
    }
    None
}

/// A child's process tree: on Windows a job object holding the child and
/// everything it starts, ended when the `Tree` is dropped or killed (and
/// when this server exits, however it exits). On Unix the callers that need
/// a tree give the child a process group and signal it themselves.
#[derive(Default)]
pub struct Tree {
    #[cfg(windows)]
    job: Option<crate::win::Job>,
}

impl Tree {
    /// The tree of the child `pid` (from `Child::id`; `None` once it exited).
    pub fn of(pid: Option<u32>) -> Tree {
        #[cfg(windows)]
        {
            let job = pid.and_then(|pid| match crate::win::Job::new(false).and_then(|job| job.assign(pid).map(|()| job)) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!("child {pid} is not in a job of its own (what it starts may outlive it): {e}");
                    None
                }
            });
            Tree { job }
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            Tree {}
        }
    }

    /// Ends the whole tree now (Windows; elsewhere nothing).
    pub fn kill(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
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
    // What a batch file started ends with it too (a timeout, a dropped task).
    #[cfg(windows)]
    let run = async {
        let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let _tree = Tree::of(child.id());
        child.wait_with_output().await
    };
    #[cfg(not(windows))]
    let run = cmd.output();
    match tokio::time::timeout(limit, run).await {
        Ok(out) => out,
        Err(_) => Err(gave_up(limit)),
    }
}

/// A command for code that is not async. Nothing kills its child for the
/// caller: run it with `output_blocking`, or own its end (a detached
/// launch, a child the caller kills itself).
#[expect(clippy::disallowed_methods, reason = "the one place commands are made")]
pub fn blocking_command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new(program);
        cmd.creation_flags(NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    std::process::Command::new(program)
}

/// `output` for code that is not async: runs `cmd` to its end with no input,
/// and kills a child still running after `limit`.
pub fn output_blocking(cmd: &mut std::process::Command, limit: Duration) -> std::io::Result<Output> {
    use std::io::Read;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    // Ended with the child, below or when this returns.
    let _tree = Tree::of(Some(child.id()));
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
    // Checked often at first, then less: a quick command (most are) is seen
    // done within a fraction of a millisecond, not rounded up to the poll.
    let mut pause = Duration::from_micros(100);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(gave_up(limit));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(5));
    };
    Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

fn gave_up(limit: Duration) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("it was still running after {} s and was stopped", limit.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child started interruptible gets Ctrl+Break and runs its handler
    /// (it exits 7 from it); this test process, on the same console, does not.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_interruptible_child_is_asked_to_stop() {
        // Short sleeps: Python runs a handler between them, and one long sleep
        // on Windows wakes early only for Ctrl+C.
        let script = "import signal, sys, time\nsignal.signal(signal.SIGBREAK, lambda *_: sys.exit(7))\nprint('ready', flush=True)\nfor _ in range(600): time.sleep(0.05)";
        let mut cmd = command("python");
        cmd.args(["-c", script]).stdout(Stdio::piped());
        if !interruptible(&mut cmd) {
            eprintln!("skipped: this test has no console to share");
            return;
        }
        let Ok(mut child) = cmd.spawn() else {
            eprintln!("skipped: python is not on PATH");
            return;
        };
        let mut ready = String::new();
        let mut out = tokio::io::BufReader::new(child.stdout.take().unwrap());
        tokio::io::AsyncBufReadExt::read_line(&mut out, &mut ready).await.unwrap();
        assert_eq!(ready.trim(), "ready");
        assert!(interrupt(child.id().unwrap()), "Ctrl+Break sent");
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await.expect("it stopped").unwrap();
        assert_eq!(status.code(), Some(7), "its handler ran");
    }

    #[test]
    fn batch_file_shims_are_run_as_what_they_start() {
        let dir = std::env::temp_dir().join(format!("domovoi-shim-{}", crate::util::new_uuid()));
        let package = dir.join("node_modules").join("@anthropic-ai").join("claude-code");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("cli.js"), "").unwrap();
        std::fs::write(dir.join("node.exe"), "").unwrap();
        // npm's cmd-shim (npm 7+).
        std::fs::write(dir.join("claude.cmd"), "@ECHO off\r\nGOTO start\r\n:start\r\nSETLOCAL\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n)\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n").unwrap();
        let shim = shim_target(&dir.join("claude.cmd")).expect("an npm shim");
        assert_eq!(shim.program, dir.join("node.exe"));
        let script = shim.script.expect("its script");
        assert!(script.ends_with("cli.js") && script.starts_with(&dir), "{}", script.display());
        // Older npm.
        std::fs::write(dir.join("old.cmd"), "@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n)\r\n").unwrap();
        assert!(shim_target(&dir.join("old.cmd")).and_then(|s| s.script).is_some_and(|s| s.ends_with("cli.js")));
        // An npm package that ships an executable (OpenCode): started as itself.
        let bin = dir.join("node_modules").join("opencode-ai").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("opencode.exe"), "").unwrap();
        std::fs::write(dir.join("opencode.cmd"), "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\opencode-ai\\bin\\opencode.exe\"   %*\r\n").unwrap();
        assert_eq!(shim_target(&dir.join("opencode.cmd")), Some(Shim { program: bin.join("opencode.exe"), script: None }));
        // A one-line wrapper, and what is no shim.
        std::fs::write(dir.join("tool.exe"), "").unwrap();
        std::fs::write(dir.join("fake"), "").unwrap();
        std::fs::write(dir.join("fake.cmd"), "@tool.exe \"%~dp0fake\" %*\r\n").unwrap();
        assert_eq!(shim_target(&dir.join("fake.cmd")), Some(Shim { program: dir.join("tool.exe"), script: Some(dir.join("fake")) }));
        // Pi's form: `@ECHO off`, then `node "%~dp0pi-launcher.js" %*` (here
        // with an interpreter beside it, so the test needs no Node on PATH).
        std::fs::write(dir.join("pi-launcher.js"), "").unwrap();
        std::fs::write(dir.join("pi.cmd"), "@ECHO off\r\ntool.exe \"%~dp0pi-launcher.js\" %*\r\n").unwrap();
        assert_eq!(shim_target(&dir.join("pi.cmd")), Some(Shim { program: dir.join("tool.exe"), script: Some(dir.join("pi-launcher.js")) }));
        std::fs::write(dir.join("other.cmd"), "@echo off\r\nset X=1\r\ncall something.bat %*\r\n").unwrap();
        assert_eq!(shim_target(&dir.join("other.cmd")), None);
        assert_eq!(shim_target(&dir.join("node.exe")), None, "not a batch file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A program that runs about 30 s, as `program, args`.
    fn sleeper() -> (&'static str, Vec<&'static str>) {
        if cfg!(windows) { ("cmd", vec!["/C", "ping -n 30 127.0.0.1 >NUL"]) } else { ("/bin/sleep", vec!["30"]) }
    }

    #[tokio::test]
    async fn a_child_that_outlives_its_limit_is_stopped() {
        let started = Instant::now();
        let (program, args) = sleeper();
        let mut cmd = command(program);
        cmd.args(&args);
        let err = output(cmd, Duration::from_millis(200)).await.expect_err("the limit ends it");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        let err = output_blocking(blocking_command(program).args(&args), Duration::from_millis(200)).expect_err("the limit ends it");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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
