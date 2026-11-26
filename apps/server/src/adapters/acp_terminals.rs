//! An ACP agent's terminals (`terminal/create`, `terminal/output`,
//! `terminal/wait_for_exit`, `terminal/kill`, `terminal/release`), after
//! t3code `provider-acp` `clientTerminals.ts`: each a command run as a
//! child of the server, its stdout and stderr kept together in a buffer.
//!
//! - The command runs in the folder the agent names, else the thread's; with
//!   the server's environment and the instance's, less the thread's identity
//!   (`DOMOVOI_THREAD_ID` and its old name), then the variables the request sets.
//!   With `shell_commands` (Devin sends shell source) a command without
//!   arguments runs in `sh -c`.
//! - Output is cut from its start (at a UTF-8 boundary) past the request's
//!   `outputByteLimit` (default 4 MiB, at most 8 MiB), and past 16 MiB over
//!   all of the session's terminals.
//! - A terminal is the session's that made it: another session's id, or a
//!   released one, is unknown. A released terminal's output is kept for the
//!   tool calls that embed it (the latest 32, 16 MiB at most).
//! - `kill` and `release` end the command (`SIGKILL`); its output is read
//!   for a second more, since a background child may hold the pipes open.
//!   Every terminal ends with the agent's session ([`Terminals::dispose_all`]).
//!
//! Limits: 16 commands running and 64 not released at once. Whether a
//! command may run at all is the adapter's call (the runtime mode, or an
//! approved command in the turn), not this module's.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "ACP `terminal/*` requests and answers, read and written as the protocol has them")]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const MAX_LIVE: usize = 16;
const MAX_UNRELEASED: usize = 64;
const MAX_RETAINED: usize = 32;
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const MAX_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// A command's output, cut from its start past `limit`.
#[derive(Debug, Default)]
struct Buffer {
    bytes: Vec<u8>,
    truncated: bool,
    limit: usize,
}

impl Buffer {
    fn append(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > self.limit {
            let excess = self.bytes.len() - self.limit;
            self.trim(excess);
        }
    }

    fn trim(&mut self, n: usize) {
        let n = n.min(self.bytes.len());
        if n > 0 {
            self.bytes.drain(..n);
            self.truncated = true;
        }
    }

    /// The text, starting at a character after a cut.
    fn text(&self) -> String {
        let mut start = 0;
        if self.truncated {
            while start < self.bytes.len() && self.bytes[start] & 0b1100_0000 == 0b1000_0000 {
                start += 1;
            }
        }
        String::from_utf8_lossy(&self.bytes[start..]).into_owned()
    }
}

struct Terminal {
    session_id: String,
    buffer: Buffer,
    /// How it ended (`TerminalExitStatus`), once it did and its output was read.
    exit: Option<Value>,
    exited: tokio::sync::watch::Receiver<Option<Value>>,
    kill: Arc<tokio::sync::Notify>,
    released: bool,
}

#[derive(Default)]
struct State {
    next: u64,
    /// By number, oldest first.
    terminals: BTreeMap<u64, Terminal>,
}

impl State {
    fn find(&mut self, session_id: &str, terminal_id: &str) -> Option<&mut Terminal> {
        let n = number(terminal_id)?;
        self.terminals.get_mut(&n).filter(|t| t.session_id == session_id)
    }

    fn live(&mut self, session_id: &str, terminal_id: &str, operation: &str) -> Result<&mut Terminal, Refused> {
        self.find(session_id, terminal_id).filter(|t| !t.released).ok_or_else(|| Refused(format!("ACP {operation} received an unknown terminal ID '{terminal_id}'.")))
    }

    /// Trims the oldest output past the total.
    fn enforce_total(&mut self) {
        let total: usize = self.terminals.values().map(|t| t.buffer.bytes.len()).sum();
        let mut excess = total.saturating_sub(MAX_RETAINED_BYTES);
        for t in self.terminals.values_mut() {
            if excess == 0 {
                break;
            }
            let before = t.buffer.bytes.len();
            t.buffer.trim(excess);
            excess -= before - t.buffer.bytes.len();
        }
    }

    /// Forgets the oldest released terminals past what is kept.
    fn prune(&mut self) {
        let released: Vec<u64> = self.terminals.iter().filter(|(_, t)| t.released).map(|(n, _)| *n).collect();
        let mut bytes: usize = released.iter().map(|n| self.terminals[n].buffer.bytes.len()).sum();
        let mut count = released.len();
        for n in released {
            if count <= MAX_RETAINED && bytes <= MAX_RETAINED_BYTES {
                break;
            }
            if let Some(t) = self.terminals.remove(&n) {
                bytes -= t.buffer.bytes.len();
                count -= 1;
            }
        }
    }
}

/// A refused request: its message (sent as JSON-RPC internal error -32603).
#[derive(Debug, PartialEq)]
pub struct Refused(pub String);

fn number(terminal_id: &str) -> Option<u64> {
    terminal_id.strip_prefix("domovoi-term-")?.parse().ok()
}

/// The name of signal `n`.
#[cfg(unix)]
fn signal_name(n: i32) -> String {
    match n {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        6 => "SIGABRT".into(),
        9 => "SIGKILL".into(),
        13 => "SIGPIPE".into(),
        15 => "SIGTERM".into(),
        n => format!("SIG{n}"),
    }
}

fn exit_status(status: std::io::Result<std::process::ExitStatus>) -> Value {
    match status {
        Ok(s) => {
            #[cfg(unix)]
            let signal = std::os::unix::process::ExitStatusExt::signal(&s).map(signal_name);
            #[cfg(not(unix))]
            let signal: Option<String> = None;
            json!({ "exitCode": s.code(), "signal": signal })
        }
        Err(_) => json!({ "exitCode": null, "signal": null }),
    }
}

/// `command` run by the platform's shell: `sh -c`, or `cmd.exe /C` on
/// Windows (`/S`: the quotes around it are taken off and the rest is run as
/// written).
fn shell(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut c = crate::process::command(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into()));
        c.args(["/D", "/S", "/C"]).raw_arg(format!("\"{command}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = crate::process::command("/bin/sh");
        c.arg("-c").arg(command);
        c
    }
}

/// One agent's terminals.
pub struct Terminals {
    state: Arc<std::sync::Mutex<State>>,
    default_cwd: PathBuf,
    /// The instance's variables, over the server's.
    env: Vec<(String, String)>,
    shell_commands: bool,
}

impl Terminals {
    pub fn new(default_cwd: &Path, env: Vec<(String, String)>, shell_commands: bool) -> Arc<Self> {
        Arc::new(Terminals { state: Arc::default(), default_cwd: default_cwd.to_path_buf(), env, shell_commands })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        crate::util::locked(&self.state)
    }

    /// `terminal/create`: starts the command; `{ terminalId }`.
    pub fn create(&self, params: &Value) -> Result<Value, Refused> {
        let session_id = params["sessionId"].as_str().unwrap_or_default().to_owned();
        let command = params["command"].as_str().filter(|c| !c.trim().is_empty()).ok_or_else(|| Refused("ACP terminal/create has no command.".into()))?;
        let args: Vec<String> = params["args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
        {
            let state = self.state();
            let unreleased = state.terminals.values().filter(|t| !t.released).count();
            if unreleased >= MAX_UNRELEASED {
                return Err(Refused(format!("ACP terminal/create exceeded the limit of {MAX_UNRELEASED} unreleased terminals.")));
            }
            if state.terminals.values().filter(|t| !t.released && t.exit.is_none()).count() >= MAX_LIVE {
                return Err(Refused(format!("ACP terminal/create exceeded the limit of {MAX_LIVE} concurrent terminals.")));
            }
        }
        let cwd = match params["cwd"].as_str().filter(|c| !c.is_empty()) {
            Some(c) => self.default_cwd.join(c),
            None => self.default_cwd.clone(),
        };
        let mut cmd = if self.shell_commands && args.is_empty() {
            shell(command)
        } else {
            let mut c = crate::process::command(command);
            c.args(&args);
            c
        };
        for (name, _) in std::env::vars_os() {
            let upper = name.to_string_lossy().to_uppercase();
            if upper == crate::terminal::THREAD_ENV || upper == crate::terminal::OLD_THREAD_ENV {
                cmd.env_remove(&name);
            }
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        for v in params["env"].as_array().into_iter().flatten() {
            if let (Some(name), Some(value)) = (v["name"].as_str(), v["value"].as_str()) {
                cmd.env(name, value);
            }
        }
        cmd.current_dir(&cwd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| Refused(format!("Could not start terminal command '{command}': {e}")))?;
        // Killing the command ends what it started too (Windows; a shell's children).
        let tree = crate::process::Tree::of(child.id());
        let limit = params["outputByteLimit"].as_u64().map_or(DEFAULT_OUTPUT_LIMIT, |l| usize::try_from(l).unwrap_or(usize::MAX)).min(MAX_OUTPUT_LIMIT);
        let (exit_tx, exited) = tokio::sync::watch::channel(None);
        let kill = Arc::new(tokio::sync::Notify::new());
        let n = {
            let mut state = self.state();
            state.next += 1;
            let n = state.next;
            state.terminals.insert(n, Terminal { session_id, buffer: Buffer { limit, ..Buffer::default() }, exit: None, exited, kill: kill.clone(), released: false });
            n
        };
        let pump = |pipe: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>| {
            let state = self.state.clone();
            tokio::spawn(async move {
                let Some(mut pipe) = pipe else { return };
                let mut chunk = vec![0u8; 16 * 1024];
                while let Ok(read) = pipe.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    let mut state = crate::util::locked(&state);
                    if let Some(t) = state.terminals.get_mut(&n) {
                        t.buffer.append(&chunk[..read]);
                    }
                    state.enforce_total();
                }
            })
        };
        let out = pump(child.stdout.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Send + Unpin>));
        let err = pump(child.stderr.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Send + Unpin>));
        let state = self.state.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s,
                () = kill.notified() => {
                    tree.kill();
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            drop(tree);
            let _ = tokio::time::timeout(DRAIN_GRACE, async {
                let _ = out.await;
                let _ = err.await;
            })
            .await;
            let status = exit_status(status);
            if let Some(t) = crate::util::locked(&state).terminals.get_mut(&n) {
                t.exit = Some(status.clone());
            }
            let _ = exit_tx.send(Some(status));
        });
        Ok(json!({ "terminalId": format!("domovoi-term-{n}") }))
    }

    /// `terminal/output`: `{ output, truncated, exitStatus? }`.
    pub fn output(&self, params: &Value) -> Result<Value, Refused> {
        let (session, id) = ids(params);
        let mut state = self.state();
        let t = state.live(session, id, "terminal/output")?;
        let mut answer = json!({ "output": t.buffer.text(), "truncated": t.buffer.truncated });
        if let Some(exit) = &t.exit {
            answer["exitStatus"] = exit.clone();
        }
        Ok(answer)
    }

    /// `terminal/wait_for_exit`: `{ exitCode, signal }` once it ended.
    pub async fn wait_for_exit(&self, params: &Value) -> Result<Value, Refused> {
        let (session, id) = ids(params);
        let mut exited = self.state().live(session, id, "terminal/wait_for_exit")?.exited.clone();
        let ended = exited.wait_for(Option::is_some).await.map(|s| s.clone()).ok().flatten();
        Ok(ended.unwrap_or_else(|| json!({ "exitCode": null, "signal": null })))
    }

    /// `terminal/kill`: ends the command, keeps the terminal.
    pub fn kill(&self, params: &Value) -> Result<Value, Refused> {
        let (session, id) = ids(params);
        self.state().live(session, id, "terminal/kill")?.kill.notify_one();
        Ok(json!({}))
    }

    /// `terminal/release`: ends the command and lets the terminal go (its output stays for embedding).
    pub async fn release(&self, params: &Value) -> Result<Value, Refused> {
        let (session, id) = ids(params);
        let mut exited = {
            let mut state = self.state();
            let t = state.live(session, id, "terminal/release")?;
            t.released = true;
            t.kill.notify_one();
            t.exited.clone()
        };
        let _ = exited.wait_for(Option::is_some).await;
        self.state().prune();
        Ok(json!({}))
    }

    /// The output of a terminal of the session, released or not (for a tool call that embeds it).
    pub fn snapshot(&self, session_id: &str, terminal_id: &str) -> Option<String> {
        self.state().find(session_id, terminal_id).map(|t| t.buffer.text())
    }

    /// Ends every command (the agent's session ends).
    pub fn dispose_all(&self) {
        let mut state = self.state();
        for t in state.terminals.values_mut() {
            t.released = true;
            t.kill.notify_one();
        }
        state.prune();
    }

    /// A tool call's `terminal` content as the text the terminal printed.
    pub fn embed(&self, session_id: &str, update: &mut Value) {
        if !matches!(update["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update")) {
            return;
        }
        let Some(content) = update["content"].as_array_mut() else { return };
        for entry in content.iter_mut().filter(|e| e["type"] == "terminal") {
            let id = entry["terminalId"].as_str().unwrap_or_default().to_owned();
            let text = self.snapshot(session_id, &id).unwrap_or_else(|| format!("[terminal {id}]"));
            *entry = json!({ "type": "content", "content": { "type": "text", "text": text } });
        }
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        self.dispose_all();
    }
}

fn ids(params: &Value) -> (&str, &str) {
    (params["sessionId"].as_str().unwrap_or_default(), params["terminalId"].as_str().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_cut_from_its_start_at_a_character() {
        let mut b = Buffer { limit: 4, ..Buffer::default() };
        b.append("aé".as_bytes());
        assert_eq!((b.text().as_str(), b.truncated), ("aé", false));
        b.append("bc".as_bytes());
        assert!(b.truncated);
        assert_eq!(b.text(), "ébc", "four bytes kept: é is two");
        b.append("d".as_bytes());
        assert_eq!(b.text(), "bcd", "a cut inside é drops what's left of it");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn a_windows_command_runs_through_cmd() {
        let dir = std::env::temp_dir().join(format!("domovoi-acp-term-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let terms = Terminals::new(&dir, vec![("FROM_INSTANCE".into(), "yes".into())], true);
        let made = terms.create(&json!({ "sessionId": "s", "command": "echo %FROM_INSTANCE% %FROM_REQUEST%& exit /b 3", "env": [{ "name": "FROM_REQUEST", "value": "too" }] })).unwrap();
        let ask = json!({ "sessionId": "s", "terminalId": made["terminalId"].as_str().unwrap() });
        assert_eq!(terms.wait_for_exit(&ask).await.unwrap(), json!({ "exitCode": 3, "signal": null }));
        assert!(terms.output(&ask).unwrap()["output"].as_str().unwrap().contains("yes too"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // POSIX shell syntax; `a_windows_command_runs_through_cmd` is its Windows twin.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_runs_and_is_read_waited_for_killed_and_released() {
        let dir = std::env::temp_dir().join(format!("domovoi-acp-term-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let terms = Terminals::new(&dir, vec![("FROM_INSTANCE".into(), "yes".into())], true);
        let made = terms.create(&json!({ "sessionId": "s", "command": "printf \"$FROM_INSTANCE $FROM_REQUEST \"; pwd; echo oops >&2; exit 3", "env": [{ "name": "FROM_REQUEST", "value": "too" }] })).unwrap();
        let id = made["terminalId"].as_str().unwrap().to_owned();
        let ask = json!({ "sessionId": "s", "terminalId": id });
        assert_eq!(terms.wait_for_exit(&ask).await.unwrap(), json!({ "exitCode": 3, "signal": null }));
        let out = terms.output(&ask).unwrap();
        let text = out["output"].as_str().unwrap();
        assert!(text.starts_with("yes too ") && text.contains(dir.file_name().unwrap().to_str().unwrap()) && text.contains("oops"), "{out}");
        assert_eq!(out["exitStatus"]["exitCode"], 3);
        assert_eq!(terms.output(&json!({ "sessionId": "other", "terminalId": id })), Err(Refused(format!("ACP terminal/output received an unknown terminal ID '{id}'."))), "another session's");

        let long = terms.create(&json!({ "sessionId": "s", "command": "sleep", "args": ["30"] })).unwrap();
        let long = json!({ "sessionId": "s", "terminalId": long["terminalId"] });
        assert!(terms.output(&long).unwrap().get("exitStatus").is_none(), "still running");
        terms.kill(&long).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(10), terms.wait_for_exit(&long)).await.expect("killed").unwrap();
        assert_eq!(ended, json!({ "exitCode": null, "signal": "SIGKILL" }));
        terms.release(&long).await.unwrap();
        assert!(terms.output(&long).is_err(), "released");

        let mut update = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "content": [{ "type": "terminal", "terminalId": id }, { "type": "terminal", "terminalId": "domovoi-term-99" }] });
        terms.embed("s", &mut update);
        assert!(update["content"][0]["content"]["text"].as_str().unwrap().starts_with("yes too"));
        assert_eq!(update["content"][1]["content"]["text"], "[terminal domovoi-term-99]");
        terms.dispose_all();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn limits_refuse_more_terminals() {
        let terms = Terminals::new(&std::env::temp_dir(), vec![], false);
        for _ in 0..MAX_LIVE {
            terms.create(&json!({ "sessionId": "s", "command": "sleep", "args": ["30"] })).unwrap();
        }
        let refused = terms.create(&json!({ "sessionId": "s", "command": "sleep", "args": ["30"] })).unwrap_err();
        assert!(refused.0.contains("16 concurrent terminals"), "{refused:?}");
        assert!(terms.create(&json!({ "sessionId": "s", "command": "  " })).is_err());
        terms.dispose_all();
    }
}
