//! PTY-backed terminals keyed by (threadId, terminalId), shared by every client.
//! Output is kept in a bounded history buffer for late attachers and fanned out
//! as `TerminalEvent`s with a per-terminal sequence.

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use domovoi_model::typed::{TerminalAttachEvent, TerminalMetadataEvent, TerminalSessionSnapshot, TerminalSessionStatus, TerminalSummary};
use tokio::sync::broadcast;

const MAX_HISTORY_BYTES: usize = 512 * 1024;
/// History grows this far past the limit before it is trimmed, so trimming
/// moves the buffer once per 128 KiB of output instead of on every read.
const HISTORY_SLACK_BYTES: usize = 128 * 1024;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;

pub type Key = (String, String);

/// Output kept for late attachers: the last `MAX_HISTORY_BYTES`, starting on
/// a character.
#[derive(Default)]
struct History(String);

impl History {
    fn tail(&self) -> &str {
        let mut cut = self.0.len().saturating_sub(MAX_HISTORY_BYTES);
        while !self.0.is_char_boundary(cut) {
            cut += 1;
        }
        &self.0[cut..]
    }
    fn push(&mut self, text: &str) {
        self.0.push_str(text);
        if self.0.len() > MAX_HISTORY_BYTES + HISTORY_SLACK_BYTES {
            let cut = self.0.len() - self.tail().len();
            self.0.drain(..cut);
        }
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

struct Session {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    label: String,
    pid: Option<u32>,
    status: TerminalSessionStatus,
    exit_code: Option<i32>,
    history: History,
    sequence: u64,
    cols: u16,
    rows: u16,
    updated_at: String,
    master: Box<dyn MasterPty + Send>,
    /// Input for the session's writer thread, in arrival order. The PTY is
    /// written there, never while `sessions` is locked: a write that waits
    /// for the program to read must not stop the output pump that lets it.
    input: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    events: broadcast::Sender<TerminalAttachEvent>,
}

/// What happened to a terminal: an event before it is numbered.
enum Happened {
    /// The terminal printed this.
    Output(String),
    /// Its shell ended, with this code when it gave one.
    Exited(Option<i32>),
    /// Its grid was resized (to the session's `cols` and `rows`).
    Resized,
    Cleared,
    Closed,
}

impl Session {
    fn snapshot(&self) -> TerminalSessionSnapshot {
        TerminalSessionSnapshot {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            worktree_path: None,
            status: self.status.clone(),
            pid: self.pid.map(u64::from),
            history: self.history.tail().to_owned(),
            exit_code: self.exit_code.map(i64::from),
            exit_signal: None,
            label: self.label.clone(),
            updated_at: self.updated_at.clone(),
            sequence: Some(self.sequence),
            cols: Some(self.cols.into()),
            rows: Some(self.rows.into()),
        }
    }
    fn summary(&self) -> TerminalSummary {
        TerminalSummary {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            worktree_path: None,
            status: self.status.clone(),
            pid: self.pid.map(u64::from),
            exit_code: self.exit_code.map(i64::from),
            exit_signal: None,
            has_running_subprocess: false,
            label: self.label.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
    /// The event of what happened, with the next number of the terminal's own count.
    fn numbered(&mut self, happened: Happened) -> TerminalAttachEvent {
        self.sequence += 1;
        let (thread_id, terminal_id, sequence) = (self.thread_id.clone(), self.terminal_id.clone(), Some(self.sequence));
        match happened {
            Happened::Output(data) => TerminalAttachEvent::Output { thread_id, terminal_id, sequence, data },
            Happened::Exited(code) => TerminalAttachEvent::Exited { thread_id, terminal_id, sequence, exit_code: code.map(i64::from), exit_signal: None },
            Happened::Resized => TerminalAttachEvent::Activity {
                thread_id,
                terminal_id,
                sequence,
                has_running_subprocess: false,
                label: self.label.clone(),
                cols: Some(self.cols.into()),
                rows: Some(self.rows.into()),
            },
            Happened::Cleared => TerminalAttachEvent::Cleared { thread_id, terminal_id, sequence },
            Happened::Closed => TerminalAttachEvent::Closed { thread_id, terminal_id, sequence },
        }
    }
    /// Numbers what happened and tells everyone attached.
    fn next_event(&mut self, happened: Happened) {
        let event = self.numbered(happened);
        let _ = self.events.send(event);
    }
}

#[derive(Clone)]
pub struct Terminals {
    sessions: Arc<Mutex<HashMap<Key, Session>>>,
    metadata: broadcast::Sender<TerminalMetadataEvent>,
}

/// A failed terminal call, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    /// The terminal's shell did not start. t3code has no error of this name:
    /// its `TerminalCwdError` is a union of three errors about the folder.
    #[error("terminal {terminal_id} of thread {thread_id} did not start: {reason}")]
    #[serde(rename = "TerminalCwdError", rename_all = "camelCase")]
    Cwd { thread_id: String, terminal_id: String, cwd: String, reason: String },

    #[error("Unknown terminal thread: {thread_id}, terminal: {terminal_id}")]
    #[serde(rename = "TerminalSessionLookupError", rename_all = "camelCase")]
    SessionLookup { thread_id: String, terminal_id: String },
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TerminalError(pub String);

impl Default for Terminals {
    fn default() -> Self {
        Terminals { sessions: Arc::default(), metadata: broadcast::channel(256).0 }
    }
}

/// The user's shell; without `$SHELL` (a service, a container) the first
/// one that exists. Windows: `DOMOVOI_SHELL`, else PowerShell 7 (`pwsh`),
/// else Windows PowerShell, else `%COMSPEC%` (cmd.exe).
fn shell() -> String {
    if cfg!(windows) {
        if let Some(s) = std::env::var("DOMOVOI_SHELL").ok().filter(|s| !s.trim().is_empty()) {
            return s;
        }
        let dirs = crate::providers::search_dirs();
        return ["pwsh", "powershell"]
            .into_iter()
            .find_map(|name| crate::providers::find_in(&dirs, name))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
    }
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && std::path::Path::new(s).exists())
        .or_else(|| ["/bin/zsh", "/bin/bash", "/bin/sh"].into_iter().find(|s| std::path::Path::new(s).exists()).map(str::to_owned))
        .unwrap_or_else(|| "/bin/sh".into())
}

/// The PTY's output, with ConPTY's first cursor position query (`ESC[6n`,
/// sent before the shell starts) answered as top left and left out. Later
/// queries are the shell's own and go to the client.
#[cfg(windows)]
struct AnswerCursorQuery {
    inner: Box<dyn Read + Send>,
    input: Option<std::sync::mpsc::Sender<Vec<u8>>>,
}

#[cfg(windows)]
impl Read for AnswerCursorQuery {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        const QUERY: &[u8] = b"\x1b[6n";
        loop {
            let n = self.inner.read(buf)?;
            let Some(input) = &self.input else { return Ok(n) };
            let Some(at) = buf[..n].windows(QUERY.len()).position(|w| w == QUERY) else { return Ok(n) };
            let _ = input.send(b"\x1b[1;1R".to_vec());
            self.input = None;
            buf.copy_within(at + QUERY.len()..n, at);
            let left = n - QUERY.len();
            if left > 0 {
                return Ok(left);
            }
        }
    }
}

/// Starts the thread that writes a session's input to its PTY. It ends when
/// the session is gone (every sender dropped) or the PTY stops taking input.
fn spawn_writer(mut writer: Box<dyn Write + Send>) -> std::sync::mpsc::Sender<Vec<u8>> {
    let (input, queued) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for data in queued {
            if writer.write_all(&data).and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    });
    input
}

impl Terminals {
    pub fn summaries(&self) -> Vec<TerminalSummary> {
        crate::util::locked(&self.sessions).values().map(Session::summary).collect()
    }

    /// `(threadId, terminalId, shell pid)` of every running terminal.
    pub fn running_pids(&self) -> Vec<(String, String, u32)> {
        let map = crate::util::locked(&self.sessions);
        map.values().filter_map(|s| Some((s.thread_id.clone(), s.terminal_id.clone(), s.pid?))).collect()
    }

    pub fn metadata_updates(&self) -> broadcast::Receiver<TerminalMetadataEvent> {
        self.metadata.subscribe()
    }

    /// Opens the terminal if needed and returns its snapshot plus an event receiver.
    pub fn open(
        &self,
        thread_id: &str,
        terminal_id: &str,
        cwd: &str,
        env: &HashMap<String, String>,
    ) -> Result<(TerminalSessionSnapshot, broadcast::Receiver<TerminalAttachEvent>), TerminalError> {
        let key = (thread_id.to_owned(), terminal_id.to_owned());
        let mut map = crate::util::locked(&self.sessions);
        if let Some(s) = map.get(&key) {
            return Ok((s.snapshot(), s.events.subscribe()));
        }
        let pair = native_pty_system()
            .openpty(PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| TerminalError(e.to_string()))?;
        let mut cmd = CommandBuilder::new(shell());
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("T3CODE_THREAD_ID", thread_id);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| TerminalError(e.to_string()))?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| TerminalError(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| TerminalError(e.to_string()))?;
        let input = spawn_writer(writer);
        // ConPTY starts by asking where the cursor is (`ESC[6n`) and starts
        // the shell only once it is told; the clients' emulators don't answer.
        #[cfg(windows)]
        let reader: Box<dyn Read + Send> = Box::new(AnswerCursorQuery { inner: reader, input: Some(input.clone()) });
        let (events, rx) = broadcast::channel(1024);
        let label = format!("Terminal {}", terminal_id.trim_start_matches("term-"));
        let session = Session {
            thread_id: thread_id.into(),
            terminal_id: terminal_id.into(),
            cwd: cwd.into(),
            label: if label.len() > 128 { "Terminal".into() } else { label },
            pid: child.process_id(),
            status: TerminalSessionStatus::Running,
            exit_code: None,
            history: History::default(),
            sequence: 1,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            updated_at: domovoi_contracts::iso_now(),
            killer: child.clone_killer(),
            master: pair.master,
            input,
            events,
        };
        let snapshot = session.snapshot();
        let _ = self.metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(session.summary()) });
        map.insert(key.clone(), session);
        drop(map);
        self.pump_output(key.clone(), reader);
        let this = self.clone();
        std::thread::spawn(move || {
            let status = child.wait().ok();
            this.on_exit(&key, status.map(|s| s.exit_code() as i32));
        });
        Ok((snapshot, rx))
    }

    fn pump_output(&self, key: Key, mut reader: Box<dyn Read + Send>) {
        let this = self.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                carry.extend_from_slice(&buf[..n]);
                // Emit only complete UTF-8; keep a trailing partial sequence.
                let valid = match std::str::from_utf8(&carry) {
                    Ok(_) => carry.len(),
                    Err(e) => e.valid_up_to(),
                };
                if valid == 0 {
                    continue;
                }
                let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                carry.drain(..valid);
                let mut map = crate::util::locked(&this.sessions);
                let Some(s) = map.get_mut(&key) else { break };
                s.history.push(&text);
                s.next_event(Happened::Output(text));
            }
        });
    }

    fn on_exit(&self, key: &Key, code: Option<i32>) {
        let mut map = crate::util::locked(&self.sessions);
        if let Some(s) = map.get_mut(key) {
            s.status = TerminalSessionStatus::Exited;
            s.exit_code = code;
            s.pid = None;
            s.updated_at = domovoi_contracts::iso_now();
            s.next_event(Happened::Exited(code));
            let _ = self.metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(s.summary()) });
        }
    }

    fn with<T>(&self, thread_id: &str, terminal_id: &str, f: impl FnOnce(&mut Session) -> T) -> Result<T, TerminalError> {
        let mut map = crate::util::locked(&self.sessions);
        let s = map
            .get_mut(&(thread_id.to_owned(), terminal_id.to_owned()))
            .ok_or_else(|| TerminalError(format!("Unknown terminal {terminal_id}")))?;
        Ok(f(s))
    }

    /// A running session's snapshot and events, without starting one.
    pub fn observe(&self, thread_id: &str, terminal_id: &str) -> Option<(TerminalSessionSnapshot, broadcast::Receiver<TerminalAttachEvent>)> {
        let map = crate::util::locked(&self.sessions);
        map.get(&(thread_id.to_owned(), terminal_id.to_owned())).map(|s| (s.snapshot(), s.events.subscribe()))
    }

    /// Queues input for the terminal. Writes reach the PTY in the order
    /// they were queued (the RPC handler queues them in arrival order).
    pub fn write(&self, thread_id: &str, terminal_id: &str, data: &str) -> Result<(), TerminalError> {
        self.with(thread_id, terminal_id, |s| s.input.send(data.as_bytes().to_vec()))?
            .map_err(|_| TerminalError(format!("Terminal {terminal_id} no longer takes input")))
    }

    pub fn resize(&self, thread_id: &str, terminal_id: &str, cols: u16, rows: u16) -> Result<(), TerminalError> {
        let metadata = self.metadata.clone();
        self.with(thread_id, terminal_id, |s| {
            s.master
                .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
                .map_err(|e| TerminalError(e.to_string()))?;
            if (s.cols, s.rows) != (cols, rows) {
                s.cols = cols;
                s.rows = rows;
                s.updated_at = domovoi_contracts::iso_now();
                s.next_event(Happened::Resized);
                let _ = metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(s.summary()) });
            }
            Ok(())
        })?
    }

    pub fn clear(&self, thread_id: &str, terminal_id: &str) -> Result<(), TerminalError> {
        self.with(thread_id, terminal_id, |s| {
            s.history.clear();
            s.next_event(Happened::Cleared);
        })
    }

    /// Closes every terminal of a thread (a deleted thread's).
    pub fn close_thread(&self, thread_id: &str) {
        let ids: Vec<String> = crate::util::locked(&self.sessions).keys().filter(|(t, _)| t == thread_id).map(|(_, id)| id.clone()).collect();
        for id in ids {
            let _ = self.close(thread_id, &id);
        }
    }

    pub fn close(&self, thread_id: &str, terminal_id: &str) -> Result<(), TerminalError> {
        let removed = crate::util::locked(&self.sessions).remove(&(thread_id.to_owned(), terminal_id.to_owned()));
        if let Some(mut s) = removed {
            let _ = s.killer.kill();
            s.next_event(Happened::Closed);
            let _ = self.metadata.send(TerminalMetadataEvent::Remove { thread_id: thread_id.to_owned(), terminal_id: terminal_id.to_owned() });
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        for (_, mut s) in crate::util::locked(&self.sessions).drain() {
            let _ = s.killer.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn runs_a_command_and_streams_output() {
        let t = Terminals::default();
        let dir = std::env::temp_dir();
        let (snap, mut rx) = t.open("th", "term-1", dir.to_str().unwrap(), &HashMap::new()).unwrap();
        assert_eq!(snap.status, TerminalSessionStatus::Running);
        assert_eq!(snap.label, "Terminal 1");
        // PowerShell on Windows (the default there), a POSIX shell elsewhere.
        let line = if cfg!(windows) { "echo \"domovoi-$(40+2)\"\r" } else { "echo domovoi-$((40+2))\n" };
        t.write("th", "term-1", line).unwrap();
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !seen.contains("domovoi-42") {
            let ev = tokio::time::timeout_at(deadline, rx.recv()).await.unwrap_or_else(|_| panic!("output in time; so far: {seen:?}")).unwrap();
            if let TerminalAttachEvent::Output { data, .. } = ev {
                seen.push_str(&data);
            }
        }
        t.resize("th", "term-1", 80, 20).unwrap();
        t.close("th", "term-1").unwrap();
        assert!(t.summaries().is_empty());
    }

    #[test]
    fn history_keeps_the_last_half_megabyte_on_a_character() {
        let mut h = History::default();
        let chunk = "aé€😀".repeat(500); // 1, 2, 3 and 4-byte characters
        let mut all = String::new();
        for _ in 0..1000 {
            h.push(&chunk);
            all.push_str(&chunk);
            let tail = h.tail();
            assert!(tail.len() <= MAX_HISTORY_BYTES);
            assert!(tail.len() > all.len().min(MAX_HISTORY_BYTES) - 4, "only a split character is left out");
            assert!(all.ends_with(tail));
            assert!(h.0.len() <= MAX_HISTORY_BYTES + HISTORY_SLACK_BYTES + chunk.len());
        }
        h.clear();
        assert_eq!(h.tail(), "");
    }

    /// A big paste into a program that echoes it back: the write must not
    /// wait on the PTY while holding what the output pump needs to drain it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_large_paste_completes_while_output_flows() {
        let t = Terminals::default();
        let dir = std::env::temp_dir();
        let (_, mut rx) = t.open("th", "term-paste", dir.to_str().unwrap(), &HashMap::new()).unwrap();
        t.write("th", "term-paste", "cat\n").unwrap();
        let line = format!("{}\n", "x".repeat(99));
        let paste = line.repeat(10_000); // 1 MB
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let writer = t.clone();
        std::thread::spawn(move || {
            let r = writer.write("th", "term-paste", &paste);
            let _ = done_tx.send(r.is_ok());
        });
        // Keep reading so the broadcast never lags while the paste goes in.
        let reading = tokio::spawn(async move { while rx.recv().await.is_ok() {} });
        let done = tokio::task::spawn_blocking(move || done_rx.recv_timeout(Duration::from_secs(30))).await.unwrap();
        assert_eq!(done, Ok(true), "the paste was taken");
        // The terminal still answers: the sessions lock is free.
        assert!(!t.summaries().is_empty());
        t.write("th", "term-paste", "\u{4}").unwrap();
        t.close("th", "term-paste").unwrap();
        reading.abort();
    }

    /// What the streams send, as JSON: a terminal that ran, was resized,
    /// cleared, ended and was closed, and the lists that followed it.
    #[tokio::test]
    async fn events_and_failures_are_written_as_t3code_reads_them() {
        use serde_json::json;
        let t = Terminals::default();
        let mut listed = t.metadata_updates();
        let dir = std::env::temp_dir();
        let cwd = dir.to_str().unwrap();
        let (snapshot, mut events) = t.open("th", "term-json", cwd, &HashMap::new()).unwrap();
        let (pid, at) = (snapshot.pid, snapshot.updated_at.clone());
        assert_eq!(
            json!(TerminalAttachEvent::Snapshot { snapshot: Box::new(snapshot) }),
            json!({ "type": "snapshot", "snapshot": {
                "threadId": "th", "terminalId": "term-json", "cwd": cwd, "worktreePath": null, "status": "running", "pid": pid, "history": "",
                "exitCode": null, "exitSignal": null, "label": "Terminal json", "updatedAt": at, "sequence": 1, "cols": 120, "rows": 30,
            } })
        );
        let summary = json!({
            "threadId": "th", "terminalId": "term-json", "cwd": cwd, "worktreePath": null, "status": "running", "pid": pid,
            "exitCode": null, "exitSignal": null, "hasRunningSubprocess": false, "label": "Terminal json", "updatedAt": at,
        });
        assert_eq!(json!(listed.recv().await.unwrap()), json!({ "type": "upsert", "terminal": summary }));
        assert_eq!(json!(TerminalMetadataEvent::Snapshot { terminals: t.summaries() }), json!({ "type": "snapshot", "terminals": [summary] }));

        // The events a session numbers, in the order made here.
        let made: Vec<serde_json::Value> = {
            let mut sessions = crate::util::locked(&t.sessions);
            let session = sessions.get_mut(&("th".to_owned(), "term-json".to_owned())).unwrap();
            let before = session.sequence;
            let happened = [Happened::Output("hi\r\n".into()), Happened::Exited(Some(3)), Happened::Exited(None), Happened::Resized, Happened::Cleared, Happened::Closed];
            let made = happened.into_iter().map(|happened| json!(session.numbered(happened))).collect();
            session.sequence = before;
            made
        };
        let named = |n: u64| json!({ "threadId": "th", "terminalId": "term-json", "sequence": n });
        let with = |mut event: serde_json::Value, n: u64| {
            event.as_object_mut().unwrap().extend(named(n).as_object().unwrap().clone());
            event
        };
        assert_eq!(made[0], with(json!({ "type": "output", "data": "hi\r\n" }), made[0]["sequence"].as_u64().unwrap()));
        let first = made[0]["sequence"].as_u64().unwrap();
        assert_eq!(made[1], with(json!({ "type": "exited", "exitCode": 3, "exitSignal": null }), first + 1));
        assert_eq!(made[2], with(json!({ "type": "exited", "exitCode": null, "exitSignal": null }), first + 2));
        assert_eq!(made[3], with(json!({ "type": "activity", "hasRunningSubprocess": false, "label": "Terminal json", "cols": 120, "rows": 30 }), first + 3));
        assert_eq!(made[4], with(json!({ "type": "cleared" }), first + 4));
        assert_eq!(made[5], with(json!({ "type": "closed" }), first + 5));

        // Resizing tells both streams; closing ends the terminal's and takes it off the list.
        t.resize("th", "term-json", 90, 20).unwrap();
        t.close("th", "term-json").unwrap();
        let mut resized = None;
        let mut last = None;
        while let Ok(event) = events.recv().await {
            if matches!(event, TerminalAttachEvent::Activity { .. }) {
                resized = Some(json!(event));
            }
            last = Some(event);
        }
        let resized = resized.expect("an activity event");
        assert_eq!((resized["cols"].as_u64(), resized["rows"].as_u64(), resized["type"].as_str()), (Some(90), Some(20), Some("activity")));
        assert!(matches!(last, Some(TerminalAttachEvent::Closed { .. })), "{last:?}");
        let mut removed = None;
        while let Ok(event) = listed.try_recv() {
            removed = Some(json!(event));
        }
        assert_eq!(removed, Some(json!({ "type": "remove", "threadId": "th", "terminalId": "term-json" })));

        assert_eq!(
            json!(Failure::Cwd { thread_id: "th".into(), terminal_id: "t1".into(), cwd: "/w".into(), reason: "no shell".into() }),
            json!({ "_tag": "TerminalCwdError", "threadId": "th", "terminalId": "t1", "cwd": "/w", "reason": "no shell" })
        );
        assert_eq!(
            json!(Failure::SessionLookup { thread_id: "th".into(), terminal_id: "t1".into() }),
            json!({ "_tag": "TerminalSessionLookupError", "threadId": "th", "terminalId": "t1" })
        );
    }
}
