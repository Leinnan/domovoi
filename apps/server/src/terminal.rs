//! PTY-backed terminals keyed by (threadId, terminalId), shared by every client.
//! Output is kept in a bounded history buffer for late attachers and fanned out
//! as `TerminalEvent`s with a per-terminal sequence.

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use domovoi_model::typed::{TerminalAttachEvent, TerminalMetadataEvent, TerminalSessionSnapshot, TerminalSessionStatus, TerminalSummary};
use tokio::sync::broadcast;

mod activity;
mod replay;
mod saved;
use replay::Replay;

const MAX_HISTORY_BYTES: usize = 512 * 1024;
/// History grows this far past the limit before it is trimmed, so trimming
/// moves the buffer once per 128 KiB of output instead of on every read.
const HISTORY_SLACK_BYTES: usize = 128 * 1024;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 30;

pub type Key = (String, String);
pub type Attachment = (TerminalSessionSnapshot, broadcast::Receiver<TerminalAttachEvent>);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    cols: u16,
    rows: u16,
}

impl Grid {
    pub fn new(cols: Option<u16>, rows: Option<u16>) -> Result<Self, TerminalError> {
        let cols = cols.unwrap_or(DEFAULT_COLS);
        let rows = rows.unwrap_or(DEFAULT_ROWS);
        if !(1..=1000).contains(&cols) || !(1..=500).contains(&rows) {
            return Err(TerminalError("Invalid terminal dimensions".into()));
        }
        Ok(Self { cols, rows })
    }

    fn pty(self) -> PtySize {
        PtySize { cols: self.cols, rows: self.rows, pixel_width: 0, pixel_height: 0 }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self { cols: DEFAULT_COLS, rows: DEFAULT_ROWS }
    }
}

fn dimension(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).unwrap_or(NonZeroU16::MIN)
}

/// Decode complete characters, replacing malformed input and holding only an
/// incomplete UTF-8 suffix for the next PTY read.
fn decode_output(carry: &mut Vec<u8>, eof: bool) -> String {
    let mut remaining = carry.as_slice();
    let mut output = String::new();
    loop {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                output.push_str(text);
                remaining = &[];
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(std::str::from_utf8(&remaining[..valid]).expect("valid UTF-8 prefix"));
                remaining = &remaining[valid..];
                if let Some(invalid) = error.error_len() {
                    output.push(char::REPLACEMENT_CHARACTER);
                    remaining = &remaining[invalid..];
                } else {
                    if eof {
                        output.push(char::REPLACEMENT_CHARACTER);
                        remaining = &[];
                    }
                    break;
                }
            }
        }
    }
    let consumed = carry.len() - remaining.len();
    carry.drain(..consumed);
    output
}

/// Output kept for late attachers: the last `MAX_HISTORY_BYTES`, starting on
/// a character.
#[derive(Default)]
struct History(String, bool);

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
            self.1 = true;
        }
    }
    fn clear(&mut self) {
        self.0.clear();
        self.1 = false;
    }
}

struct Viewer {
    streams: usize,
    grid: Grid,
}

struct Launch<'a> {
    cwd: &'a str,
    env: &'a HashMap<String, String>,
    grid: Grid,
    restore: Option<(String, String)>,
}

struct Session {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    shell: String,
    label: String,
    pid: Option<u32>,
    status: TerminalSessionStatus,
    exit_code: Option<i32>,
    history: History,
    replay: Replay,
    screen: vt100::Parser<activity::ScreenMetadata>,
    agent: Option<activity::Presence>,
    agent_thread_id: Option<String>,
    #[cfg(unix)]
    agent_revision: u64,
    instance_id: String,
    resize_since_clear: bool,
    restored: bool,
    saved_screen: Option<String>,
    captured_sequence: u64,
    viewers: HashMap<u64, Viewer>,
    sequence: u64,
    cols: u16,
    rows: u16,
    updated_at: String,
    master: Option<Box<dyn MasterPty + Send>>,
    /// Input for the session's writer thread, in arrival order. The PTY is
    /// written there, never while `sessions` is locked: a write that waits
    /// for the program to read must not stop the output pump that lets it.
    input: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
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
    fn snapshot(&self, redraw: bool) -> TerminalSessionSnapshot {
        let screen = self.screen.screen();
        let history = self.history.tail();
        let truncated = self.history.1 || self.history.0.len() > MAX_HISTORY_BYTES;
        // Stored control tokens never contain newlines, so the first whole
        // line after eviction starts outside any sequence that was cut.
        let mut history = if truncated {
            history.split_once('\n').map_or("", |(_, rest)| rest).to_owned()
        } else {
            history.to_owned()
        };
        if redraw || truncated || screen.alternate_screen() || self.resize_since_clear {
            replay::append_redraw(&mut history, screen);
        }
        history.push_str(self.replay.pending());
        if self.restored {
            history = self.saved_screen.as_deref().unwrap_or_default().replace('\n', "\r\n");
        }
        TerminalSessionSnapshot {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            worktree_path: None,
            status: self.status.clone(),
            pid: self.pid.map(u64::from),
            history,
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
    fn viewer_grid(&self) -> Option<Grid> {
        let cols = self.viewers.values().map(|viewer| viewer.grid.cols).min()?;
        let rows = self.viewers.values().map(|viewer| viewer.grid.rows).min()?;
        Some(Grid { cols, rows })
    }

    fn resize(&mut self, grid: Grid, metadata: &broadcast::Sender<TerminalMetadataEvent>) -> Result<(), TerminalError> {
        if !self.restored && (self.cols, self.rows) != (grid.cols, grid.rows) {
            if let Some(master) = &self.master {
                master.resize(grid.pty()).map_err(|e| TerminalError(e.to_string()))?;
            }
            self.cols = grid.cols;
            self.rows = grid.rows;
            self.screen.set_size(dimension(grid.rows), dimension(grid.cols));
            self.resize_since_clear = true;
            self.updated_at = crate::time_fmt::iso_now();
            self.next_event(Happened::Resized);
            let _ = metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(self.summary()) });
        }
        Ok(())
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
    db: Option<crate::db::Db>,
    save_screens: Arc<std::sync::atomic::AtomicBool>,
    details: broadcast::Sender<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDetail {
    thread_id: String,
    terminal_id: String,
    instance_id: String,
    cwd: String,
    reported_cwd: Option<String>,
    label: String,
    title: Option<String>,
    running: bool,
    exit_code: Option<i32>,
    restored: bool,
    has_saved_screen: bool,
    agent: Option<activity::Presence>,
    agent_thread_id: Option<String>,
    updated_at: String,
}

/// Keeps one interactive stream registered until the RPC stream is dropped.
pub struct InteractiveViewer {
    terminals: Terminals,
    key: Key,
    instance: broadcast::Receiver<TerminalAttachEvent>,
    connection: u64,
}

impl Drop for InteractiveViewer {
    fn drop(&mut self) {
        let mut sessions = crate::util::locked(&self.terminals.sessions);
        let Some(session) = sessions.get_mut(&self.key) else { return };
        if !session.events.subscribe().same_channel(&self.instance) {
            return;
        }
        if let Some(viewer) = session.viewers.get_mut(&self.connection) {
            viewer.streams -= 1;
            if viewer.streams == 0 {
                session.viewers.remove(&self.connection);
                if let Some(grid) = session.viewer_grid() {
                    if session.resize(grid, &self.terminals.metadata).is_ok() {
                        self.terminals.persist(session);
                    }
                }
            }
        }
    }
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
        Terminals { sessions: Arc::default(), metadata: broadcast::channel(256).0, db: None, save_screens: Arc::default(), details: broadcast::channel(16).0 }
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
/// sent before the shell starts) answered using a temporary startup screen. Later
/// queries are the shell's own and go to the client.
#[cfg(any(windows, test))]
struct AnswerCursorQuery {
    inner: Box<dyn Read + Send>,
    input: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    pending: Vec<u8>,
    screen: vt100::Parser,
}

#[cfg(any(windows, test))]
impl Read for AnswerCursorQuery {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        const QUERY: &[u8] = b"\x1b[6n";
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pending.len() >= buf.len() || self.input.is_none() && !self.pending.is_empty() {
                let len = self.pending.len().min(buf.len());
                buf[..len].copy_from_slice(&self.pending[..len]);
                self.pending.drain(..len);
                return Ok(len);
            }
            if self.input.is_none() {
                return self.inner.read(buf);
            }
            let count = self.inner.read(buf)?;
            self.pending.extend_from_slice(&buf[..count]);
            if let Some(at) = self.pending.windows(QUERY.len()).position(|part| part == QUERY) {
                self.screen.process(&self.pending[..at]);
                let (row, col) = self.screen.screen().cursor_position();
                self.pending.drain(at..at + QUERY.len());
                if let Some(input) = self.input.take() {
                    let _ = input.send(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
                }
            }
            let hold = if count > 0 && self.input.is_some() {
                (1..QUERY.len()).rev().find(|&len| self.pending.ends_with(&QUERY[..len])).unwrap_or(0)
            } else {
                0
            };
            let available = (self.pending.len() - hold).min(buf.len());
            if available > 0 || count == 0 {
                buf[..available].copy_from_slice(&self.pending[..available]);
                if self.input.is_some() {
                    self.screen.process(&buf[..available]);
                }
                self.pending.drain(..available);
                return Ok(available);
            }
        }
    }
}

/// Starts the thread that writes a session's input to its PTY. It ends when
/// the session is gone (every sender dropped) or the PTY stops taking input.
fn spawn_writer(mut writer: Box<dyn Write + Send>) -> std::sync::mpsc::SyncSender<Vec<u8>> {
    let (input, queued) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
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
    pub fn load(db: crate::db::Db, save_screens: bool) -> rusqlite::Result<Self> {
        if !save_screens {
            saved::erase_screens(&db)?;
        }
        let sessions = saved::load(&db)?.into_iter().map(|item| {
            let key = (item.thread.clone(), item.id.clone());
            let session = Session {
                thread_id: item.thread,
                terminal_id: item.id,
                cwd: item.cwd,
                shell: item.shell,
                label: item.label,
                pid: None,
                status: TerminalSessionStatus::Exited,
                exit_code: item.exit_code,
                history: History::default(),
                replay: Replay::default(),
                // A saved screen is displayed as text. No live emulator or
                // scrollback is needed for a descriptor until it is reopened.
                screen: vt100::Parser::new_with_callbacks(NonZeroU16::MIN, NonZeroU16::MIN, 0, activity::ScreenMetadata { cwd: item.reported_cwd, title: None }),
                agent: None,
                agent_thread_id: None,
                #[cfg(unix)]
                agent_revision: 0,
                instance_id: crate::util::new_uuid(),
                resize_since_clear: false,
                restored: true,
                saved_screen: item.screen,
                captured_sequence: 1,
                viewers: HashMap::new(),
                sequence: 1,
                cols: item.cols.clamp(1, 1000),
                rows: item.rows.clamp(1, 500),
                updated_at: item.updated_at,
                master: None,
                input: None,
                killer: None,
                events: broadcast::channel(1).0,
            };
            (key, session)
        }).collect();
        Ok(Self {
            sessions: Arc::new(Mutex::new(sessions)),
            db: Some(db),
            save_screens: Arc::new(std::sync::atomic::AtomicBool::new(save_screens)),
            ..Self::default()
        })
    }

    pub fn configure_screen_saving(&self, enabled: bool) -> rusqlite::Result<()> {
        let mut sessions = crate::util::locked(&self.sessions);
        self.save_screens.store(enabled, std::sync::atomic::Ordering::Relaxed);
        if !enabled {
            for session in sessions.values_mut() {
                if session.saved_screen.take().is_some() {
                    session.next_event(Happened::Cleared);
                }
                session.captured_sequence = 0;
            }
            let _ = self.details.send(());
            if let Some(db) = &self.db {
                saved::erase_screens(db)?;
            }
        }
        Ok(())
    }

    pub fn capture_screens(&self) {
        let mut sessions = crate::util::locked(&self.sessions);
        for session in sessions.values_mut() {
            self.capture(session);
        }
    }

    fn capture(&self, session: &mut Session) {
        let Some(db) = &self.db else { return };
        if !session.restored && session.captured_sequence != session.sequence
            && self.save_screens.load(std::sync::atomic::Ordering::Relaxed)
        {
            match saved::capture(db, session) {
                Ok(()) => session.captured_sequence = session.sequence,
                Err(error) => tracing::warn!(%error, "Could not save terminal screen"),
            }
        }
    }

    fn persist(&self, session: &Session) {
        if let Some(db) = &self.db
            && let Err(error) = saved::save(db, session)
        {
            tracing::warn!(%error, "Could not save terminal descriptor");
        }
    }

    pub fn attention_summaries(&self) -> Vec<TerminalDetail> {
        self.details().into_iter().filter_map(|mut terminal| {
            if terminal.agent.is_none() && !terminal.exit_code.is_some_and(|code| code != 0) {
                return None;
            }
            terminal.cwd.clear();
            terminal.reported_cwd = None;
            terminal.has_saved_screen = false;
            Some(terminal)
        }).collect()
    }

    pub fn details_updates(&self) -> broadcast::Receiver<()> {
        self.details.subscribe()
    }

    pub fn details(&self) -> Vec<TerminalDetail> {
        let sessions = crate::util::locked(&self.sessions);
        let mut details: Vec<_> = sessions.values().map(|s| TerminalDetail {
            thread_id: s.thread_id.clone(),
            terminal_id: s.terminal_id.clone(),
            instance_id: s.instance_id.clone(),
            cwd: s.cwd.clone(),
            reported_cwd: s.screen.callbacks().cwd.clone(),
            label: s.label.clone(),
            title: s.screen.callbacks().title.clone(),
            running: s.pid.is_some(),
            exit_code: s.exit_code,
            restored: s.restored,
            has_saved_screen: s.saved_screen.is_some(),
            agent: s.agent,
            agent_thread_id: s.agent_thread_id.clone(),
            updated_at: s.updated_at.clone(),
        }).collect();
        details.sort_by(|a, b| a.thread_id.cmp(&b.thread_id).then_with(|| a.terminal_id.cmp(&b.terminal_id)));
        details
    }

    #[cfg(unix)]
    pub async fn sample_activity(&self) {
        // Collect groups before sampling process names, then recheck the
        // controlling group and terminal instance before applying a result.
        let targets: Vec<_> = crate::util::locked(&self.sessions).iter().filter_map(|(key, session)| {
            let group = session.master.as_ref()?.process_group_leader()?;
            Some((key.clone(), session.events.clone(), group))
        }).collect();
        if targets.is_empty() {
            return;
        }
        let command = crate::process::command("ps").args(["-A", "-o", "pgid=,comm=,args="]).kill_on_drop(true).output();
        let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command).await else { return };
        if !output.status.success() {
            return;
        }
        let groups = activity::groups(&String::from_utf8_lossy(&output.stdout));
        let mut sessions = crate::util::locked(&self.sessions);
        let mut changed = false;
        for (key, instance, group) in targets {
            let Some(session) = sessions.get_mut(&key).filter(|session| session.events.same_channel(&instance)) else { continue };
            if session.master.as_ref().and_then(|master| master.process_group_leader()) != Some(group) {
                continue;
            }
            let prior = session.agent;
            let agent = groups.get(&group);
            let linked = agent.and_then(|candidate| {
                saved::thread_for_session(self.db.as_ref()?, candidate.driver(), candidate.native_session.as_deref()?).ok().flatten()
            });
            if session.agent_thread_id != linked {
                session.agent_thread_id = linked;
                changed = true;
            }
            let next = agent.map(|candidate| {
                let agent = candidate.agent;
                let state = activity::classify(agent, &session.screen.screen().contents(), prior.filter(|p| p.agent == agent).map(|p| p.activity));
                activity::Presence { agent, activity: state, revision: session.agent_revision + 1 }
            });
            if prior.map(|p| (p.agent, p.activity)) != next.map(|p| (p.agent, p.activity)) {
                session.agent_revision += 1;
                session.agent = next;
                session.updated_at = crate::time_fmt::iso_now();
                changed = true;
            }
        }
        if changed {
            let _ = self.details.send(());
        }
    }

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
    ) -> Result<Attachment, TerminalError> {
        self.open_sized(thread_id, terminal_id, cwd, env, Grid::default())
    }

    pub fn open_sized(
        &self,
        thread_id: &str,
        terminal_id: &str,
        cwd: &str,
        env: &HashMap<String, String>,
        grid: Grid,
    ) -> Result<Attachment, TerminalError> {
        self.launch(thread_id, terminal_id, Launch { cwd, env, grid, restore: None })
    }

    pub fn reopen(&self, thread_id: &str, old_id: &str, new_id: &str) -> Result<TerminalSessionSnapshot, TerminalError> {
        let (cwd, grid, shell, label) = self.with(thread_id, old_id, |session| {
            if session.pid.is_some() {
                return Err(TerminalError("This terminal is still running".into()));
            }
            Ok((session.cwd.clone(), Grid { cols: session.cols, rows: session.rows }, session.shell.clone(), session.label.clone()))
        })??;
        self.launch(thread_id, new_id, Launch { cwd: &cwd, env: &HashMap::new(), grid, restore: Some((shell, label)) })
            .map(|(snapshot, _)| snapshot)
    }

    pub fn rename(&self, thread_id: &str, terminal_id: &str, label: &str) -> Result<(), TerminalError> {
        let label = label.trim();
        if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
            return Err(TerminalError("Enter a terminal name up to 128 bytes".into()));
        }
        self.with(thread_id, terminal_id, |session| {
            let previous = std::mem::replace(&mut session.label, label.to_owned());
            if let Some(db) = &self.db
                && let Err(error) = saved::save(db, session)
            {
                session.label = previous;
                return Err(TerminalError(error.to_string()));
            }
            let _ = self.details.send(());
            let _ = self.metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(session.summary()) });
            Ok(())
        })?
    }

    fn launch(&self, thread_id: &str, terminal_id: &str, launch: Launch<'_>) -> Result<Attachment, TerminalError> {
        let Launch { cwd, env, grid, restore } = launch;
        let key = (thread_id.to_owned(), terminal_id.to_owned());
        let mut map = crate::util::locked(&self.sessions);
        if let Some(s) = map.get(&key) {
            if restore.is_some() {
                return Err(TerminalError("A terminal with the new ID already exists".into()));
            }
            return Ok((s.snapshot(false), s.events.subscribe()));
        }
        let pair = native_pty_system()
            .openpty(grid.pty())
            .map_err(|e| TerminalError(e.to_string()))?;
        let (program, label) = restore.unwrap_or_else(|| (shell(), format!("Terminal {}", terminal_id.trim_start_matches("term-"))));
        let mut cmd = CommandBuilder::new(&program);
        // A login shell; Windows shells have no such flag (Windows
        // PowerShell refuses `-l`).
        if !cfg!(windows) {
            cmd.arg("-l");
        }
        // `cmd.exe` can't start in a `\\?\C:\…` folder.
        cmd.cwd(crate::util::plain_path(std::path::Path::new(cwd)));
        cmd.env("TERM", "xterm-256color");
        cmd.env("T3CODE_THREAD_ID", thread_id);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let reader = pair.master.try_clone_reader().map_err(|e| TerminalError(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| TerminalError(e.to_string()))?;
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| TerminalError(e.to_string()))?;
        drop(pair.slave);
        let input = spawn_writer(writer);
        // ConPTY starts by asking where the cursor is (`ESC[6n`) and starts
        // the shell only once it is told; the clients' emulators don't answer.
        #[cfg(windows)]
        let reader: Box<dyn Read + Send> = Box::new(AnswerCursorQuery {
            inner: reader, input: Some(input.clone()), pending: Vec::new(),
            screen: vt100::Parser::new(dimension(grid.rows), dimension(grid.cols), 0),
        });
        let (events, rx) = broadcast::channel(1024);
        let session = Session {
            thread_id: thread_id.into(),
            terminal_id: terminal_id.into(),
            cwd: cwd.into(),
            shell: program,
            label: if label.len() > 128 { "Terminal".into() } else { label },
            pid: child.process_id(),
            status: TerminalSessionStatus::Running,
            exit_code: None,
            history: History::default(),
            replay: Replay::default(),
            screen: vt100::Parser::new_with_callbacks(dimension(grid.rows), dimension(grid.cols), 0, activity::ScreenMetadata::default()),
            agent: None,
            agent_thread_id: None,
            #[cfg(unix)]
            agent_revision: 0,
            instance_id: crate::util::new_uuid(),
            resize_since_clear: false,
            restored: false,
            saved_screen: None,
            captured_sequence: 0,
            viewers: HashMap::new(),
            sequence: 1,
            cols: grid.cols,
            rows: grid.rows,
            updated_at: crate::time_fmt::iso_now(),
            killer: Some(child.clone_killer()),
            master: Some(pair.master),
            input: Some(input),
            events,
        };
        let snapshot = session.snapshot(false);
        let instance = session.events.clone();
        self.persist(&session);
        let _ = self.metadata.send(TerminalMetadataEvent::Upsert { terminal: Box::new(session.summary()) });
        map.insert(key.clone(), session);
        let _ = self.details.send(());
        drop(map);
        let (drained, drain) = std::sync::mpsc::channel();
        self.pump_output(key.clone(), instance.clone(), reader, drained);
        let this = self.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code() as i32);
            // Closing ConPTY can require the reader to keep draining.
            let master = {
                let mut map = crate::util::locked(&this.sessions);
                let Some(s) = map.get_mut(&key).filter(|s| s.events.same_channel(&instance)) else { return };
                s.input.take();
                s.master.take()
            };
            if let Some(master) = master {
                #[cfg(windows)]
                std::thread::spawn(move || drop(master));
                #[cfg(not(windows))]
                drop(master);
            }
            let _ = drain.recv_timeout(Duration::from_secs(2));
            this.on_exit(&key, &instance, code);
        });
        Ok((snapshot, rx))
    }

    fn pump_output(
        &self,
        key: Key,
        instance: broadcast::Sender<TerminalAttachEvent>,
        mut reader: Box<dyn Read + Send>,
        drained: std::sync::mpsc::Sender<()>,
    ) {
        let this = self.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut carry = Vec::new();
            loop {
                let count = match reader.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result.unwrap_or(0),
                };
                carry.extend_from_slice(&buf[..count]);
                let text = decode_output(&mut carry, count == 0);
                if !text.is_empty() {
                    let mut map = crate::util::locked(&this.sessions);
                    let Some(s) = map.get_mut(&key).filter(|s| s.events.same_channel(&instance) && s.pid.is_some()) else { break };
                    let (title, cwd) = (s.screen.callbacks().title.clone(), s.screen.callbacks().cwd.clone());
                    s.screen.process(text.as_bytes());
                    if s.screen.callbacks().title != title || s.screen.callbacks().cwd != cwd {
                        s.updated_at = crate::time_fmt::iso_now();
                        if s.screen.callbacks().cwd != cwd {
                            this.persist(s);
                        }
                        let _ = this.details.send(());
                    }
                    s.replay.push(&mut s.history, &text);
                    s.next_event(Happened::Output(text));
                }
                if count == 0 {
                    break;
                }
            }
            let _ = drained.send(());
        });
    }

    fn on_exit(&self, key: &Key, instance: &broadcast::Sender<TerminalAttachEvent>, code: Option<i32>) {
        let mut map = crate::util::locked(&self.sessions);
        if let Some(s) = map.get_mut(key).filter(|s| s.events.same_channel(instance)) {
            s.status = TerminalSessionStatus::Exited;
            s.exit_code = code;
            s.pid = None;
            s.agent = None;
            s.agent_thread_id = None;
            let _ = self.details.send(());
            s.updated_at = crate::time_fmt::iso_now();
            s.next_event(Happened::Exited(code));
            self.persist(s);
            self.capture(s);
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
    pub fn observe(&self, thread_id: &str, terminal_id: &str) -> Option<Attachment> {
        let map = crate::util::locked(&self.sessions);
        map.get(&(thread_id.to_owned(), terminal_id.to_owned())).map(|s| (s.snapshot(false), s.events.subscribe()))
    }

    pub fn recover(&self, key: &Key, previous: &broadcast::Receiver<TerminalAttachEvent>) -> Option<Attachment> {
        let map = crate::util::locked(&self.sessions);
        let session = map.get(key)?;
        let rx = session.events.subscribe();
        if !rx.same_channel(previous) {
            return None;
        }
        Some((session.snapshot(true), rx))
    }

    pub fn attach_viewer(&self, attachment: Attachment, connection: u64, grid: Grid) -> Result<(Attachment, InteractiveViewer), TerminalError> {
        let (snapshot, instance) = attachment;
        let key = (snapshot.thread_id, snapshot.terminal_id);
        let mut sessions = crate::util::locked(&self.sessions);
        let session = sessions.get_mut(&key).filter(|session| session.events.subscribe().same_channel(&instance))
            .ok_or_else(|| TerminalError("Terminal closed during attach".into()))?;
        let viewer = session.viewers.entry(connection).or_insert(Viewer { streams: 0, grid });
        viewer.streams += 1;
        let effective = session.viewer_grid().unwrap_or(grid);
        if let Err(error) = session.resize(effective, &self.metadata) {
            let viewer = session.viewers.get_mut(&connection).expect("registered viewer");
            viewer.streams -= 1;
            if viewer.streams == 0 {
                session.viewers.remove(&connection);
            }
            return Err(error);
        }
        self.persist(session);
        let attachment = (session.snapshot(false), session.events.subscribe());
        Ok((attachment, InteractiveViewer { terminals: self.clone(), key, instance, connection }))
    }

    /// Queues input for the terminal. Writes reach the PTY in the order
    /// they were queued (the RPC handler queues them in arrival order).
    pub fn write(&self, thread_id: &str, terminal_id: &str, data: &str) -> Result<(), TerminalError> {
        // Bound queued bytes without waiting under the session lock.
        if data.len() > 1024 * 1024 {
            return Err(TerminalError("Terminal input exceeds 1 MiB".into()));
        }
        self.with(thread_id, terminal_id, |s| {
            let input = s.input.as_ref().ok_or_else(|| TerminalError(format!("Terminal {terminal_id} no longer takes input")))?;
            input.try_send(data.as_bytes().to_vec()).map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => TerminalError(format!("Terminal {terminal_id} input queue is full")),
                std::sync::mpsc::TrySendError::Disconnected(_) => TerminalError(format!("Terminal {terminal_id} no longer takes input")),
            })
        })?
    }

    #[cfg(test)]
    pub fn resize(&self, thread_id: &str, terminal_id: &str, cols: u16, rows: u16) -> Result<(), TerminalError> {
        self.resize_viewer(thread_id, terminal_id, cols, rows, None)
    }

    pub fn resize_viewer(&self, thread_id: &str, terminal_id: &str, cols: u16, rows: u16, connection: Option<u64>) -> Result<(), TerminalError> {
        let grid = Grid::new(Some(cols), Some(rows))?;
        self.with(thread_id, terminal_id, |session| {
            if let Some(viewer) = connection.and_then(|id| session.viewers.get_mut(&id)) {
                viewer.grid = grid;
            }
            let sequence = session.sequence;
            session.resize(session.viewer_grid().unwrap_or(grid), &self.metadata)?;
            if session.sequence != sequence {
                self.persist(session);
            }
            Ok(())
        })?
    }

    pub fn clear(&self, thread_id: &str, terminal_id: &str) -> Result<(), TerminalError> {
        self.with(thread_id, terminal_id, |s| {
            s.history.clear();
            s.saved_screen = None;
            s.replay = Replay::default();
            s.screen.process(b"\x1b[H\x1b[2J");
            s.resize_since_clear = false;
            s.next_event(Happened::Cleared);
            self.capture(s);
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
        let removed = {
            let mut sessions = crate::util::locked(&self.sessions);
            if let Some(db) = &self.db {
                saved::remove(db, thread_id, terminal_id).map_err(|error| TerminalError(error.to_string()))?;
            }
            let removed = sessions.remove(&(thread_id.to_owned(), terminal_id.to_owned()));
            if removed.is_some() {
                let _ = self.details.send(());
                let _ = self.metadata.send(TerminalMetadataEvent::Remove { thread_id: thread_id.to_owned(), terminal_id: terminal_id.to_owned() });
            }
            removed
        };
        if let Some(mut s) = removed {
            if let Some(killer) = &mut s.killer { let _ = killer.kill(); }
            s.next_event(Happened::Closed);
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        self.capture_screens();
        let sessions = std::mem::take(&mut *crate::util::locked(&self.sessions));
        for (_, mut s) in sessions {
            if let Some(killer) = &mut s.killer { let _ = killer.kill(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn decodes_invalid_and_split_utf8_without_holding_later_output() {
        let mut bytes = b"ok\xff\xf0\x9f".to_vec();
        assert_eq!(decode_output(&mut bytes, false), "ok�");
        assert_eq!(bytes, b"\xf0\x9f");
        bytes.extend_from_slice(b"\x98\x80done");
        assert_eq!(decode_output(&mut bytes, false), "😀done");
        assert!(bytes.is_empty());
    }

    #[test]
    fn answers_a_conpty_startup_query_split_between_reads() {
        struct Chunks(std::vec::IntoIter<&'static [u8]>);
        impl Read for Chunks {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let chunk = self.0.next().unwrap_or_default();
                buffer[..chunk.len()].copy_from_slice(chunk);
                Ok(chunk.len())
            }
        }
        let (input, replies) = std::sync::mpsc::sync_channel(1);
        let chunks: Vec<&[u8]> = vec![b"pre\x1b[", b"6", b"npost"];
        let mut reader = AnswerCursorQuery { inner: Box::new(Chunks(chunks.into_iter())), input: Some(input), pending: Vec::new(), screen: vt100::Parser::default() };
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert_eq!(output, "prepost");
        assert_eq!(replies.recv().unwrap(), b"\x1b[1;4R");
    }

    #[test]
    fn interactive_viewers_share_a_grid_and_observers_do_not_change_it() {
        let terminals = Terminals::default();
        let dir = std::env::temp_dir();
        let env = HashMap::new();
        let large = Grid::new(Some(100), Some(40)).unwrap();
        let small = Grid::new(Some(50), Some(30)).unwrap();
        let attach = terminals.open_sized("thread", "terminal", dir.to_str().unwrap(), &env, large).unwrap();
        let (_, desktop) = terminals.attach_viewer(attach, 1, large).unwrap();
        let observer = terminals.observe("thread", "terminal").unwrap();
        assert_eq!(observer.0.cols, Some(100));
        let (phone, controller) = terminals.attach_viewer(observer, 2, small).unwrap();
        assert_eq!((phone.0.cols, phone.0.rows), (Some(50), Some(30)));
        drop(controller);
        assert_eq!(terminals.observe("thread", "terminal").unwrap().0.cols, Some(100));
        drop(desktop);
        terminals.shutdown();
    }

    #[test]
    fn restoring_keeps_old_descriptors_ended_and_purges_disabled_screens() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let terminals = Terminals::load(db.clone(), true).unwrap();
        let dir = std::env::temp_dir();
        let (_, _) = terminals.open("thread", "terminal", dir.to_str().unwrap(), &HashMap::new()).unwrap();
        terminals.capture_screens();
        terminals.shutdown();

        let restored = Terminals::load(db.clone(), true).unwrap();
        let (snapshot, _) = restored.observe("thread", "terminal").unwrap();
        assert_eq!(snapshot.status, TerminalSessionStatus::Exited);
        assert_eq!(snapshot.pid, None);
        assert!(saved::load(&db).unwrap()[0].screen.is_some());
        restored.configure_screen_saving(false).unwrap();
        assert!(saved::load(&db).unwrap()[0].screen.is_none());
        restored.close("thread", "terminal").unwrap();
        assert!(saved::load(&db).unwrap().is_empty());
    }

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
