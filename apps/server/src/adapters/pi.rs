//! Pi adapter over `pi --mode rpc` (after t3code `provider-pi` `adapter.ts`).
//! One pi process per thread, kept alive between turns as Claude's is, and
//! restarted when the thread's runtime mode changes (the bundled extension
//! reads the mode once).
//!
//! A thread's native id is pi's session file; a turn's is the id of its user
//! entry in pi's session tree, read after the turn (`get_entries`), which
//! rollback and fork cut at. Until then a turn carries a `pending:` ref.
//!
//! A turn is a `prompt`; it ends when pi settles (`agent_settled`, then a
//! `get_state` that shows nothing streaming, compacting or queued). Approvals
//! and questions come from the extension's dialogs (`extension_ui_request`).
//! `/compact` runs pi's `compact` instead of a prompt.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "pi's RPC records, tool arguments and results, and session entries")]

use super::turn::{Turn, TurnEnd, WindowUse, Work, decision_allows};
use super::{Control, TurnInput};
use crate::orchestration::commands::Answers;
use crate::pi_rpc::{DEFAULT_TIMEOUT, LIFECYCLE_TIMEOUT, Launch, PiEvent, PiRpc, SHORT_TIMEOUT};
use domovoi_model::typed::{FileChangeDetail, ProviderRequestKind, ProviderSessionStatus, ProviderThreadStatus, QuestionOption, TurnItemStatus, UserInputQuestion};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};

const EXTENSION_SOURCE: &str = include_str!("pi_extension.ts");
pub const EXTENSION_FILE: &str = "domovoi-pi-extension.ts";
/// Where the extension finds itself (to tell its own tools from a user extension's).
pub const EXTENSION_PATH_ENV: &str = "DOMOVOI_PI_EXTENSION_PATH";
/// A turn's ref before its user entry is known: never a pi entry id.
pub const PENDING_REF: &str = "pending:";
const SETTLE_PROBES: u32 = 3;
const SETTLE_RETRY: Duration = Duration::from_millis(100);
/// pi refuses a prompt that races a compaction it already started.
const COMPACTING_REJECTION: &str = "Cannot submit a prompt while compaction is in progress";
const IDLE_DECLINED: &str = "Pi asked for something outside a Domovoi turn, where nobody could answer.";

/// Writes the bundled extension to `<userdata>/caches` (staged, then renamed,
/// so a pi starting meanwhile never reads half a file); returns its path.
pub fn write_extension(userdata: &Path) -> Result<PathBuf, String> {
    let dir = userdata.join("caches");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(EXTENSION_FILE);
    let source = EXTENSION_SOURCE.replace("__DOMOVOI_MCP_SERVER__", crate::mcp::SERVER_NAME);
    if std::fs::read_to_string(&path).is_ok_and(|s| s == source) {
        return Ok(path);
    }
    let staged = dir.join(format!(".{EXTENSION_FILE}.{}", crate::util::new_uuid()));
    std::fs::write(&staged, &source).and_then(|()| std::fs::rename(&staged, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("could not write the pi extension: {e}")
    })?;
    Ok(path)
}

/// `/compact` or `/compact <instructions>` (not `/compacted`): `Some(instructions)`.
pub fn parse_compact(text: &str) -> Option<Option<String>> {
    let rest = text.trim().strip_prefix("/compact")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    Some((!rest.is_empty()).then(|| rest.to_owned()))
}

// ---------------------------------------------------------------------------
// The session tree (`get_entries`)
// ---------------------------------------------------------------------------

pub struct Entry {
    pub parent: Option<String>,
    pub user: bool,
}

/// A `get_entries` answer: entries by id, and the active leaf.
pub struct Entries {
    pub by_id: HashMap<String, Entry>,
    pub leaf: Option<String>,
}

/// `None` for an answer that isn't a tree: an entry without an id, an id
/// twice, or a parent that isn't an id.
pub fn entries(data: &Value) -> Option<Entries> {
    let mut by_id = HashMap::new();
    for e in data["entries"].as_array()? {
        let id = e["id"].as_str()?.to_owned();
        let parent = match &e["parentId"] {
            Value::Null => None,
            Value::String(p) => Some(p.clone()),
            _ => return None,
        };
        let user = e["type"] == "message" && e["message"]["role"] == "user";
        if by_id.insert(id.clone(), Entry { parent, user }).is_some() {
            return None;
        }
    }
    Some(Entries { by_id, leaf: data["leafId"].as_str().map(str::to_owned) })
}

pub struct Walk {
    /// User entries from the leaf back, newest first.
    pub user_entries: Vec<String>,
    pub reached_stop: bool,
}

/// From the leaf back through parents until `stop` (excluded) or the root.
/// `None` on a cycle.
pub fn walk(tree: &Entries, stop: Option<&str>) -> Option<Walk> {
    let mut seen = HashSet::new();
    let mut user_entries = vec![];
    let mut current = tree.leaf.clone();
    let reached_stop = loop {
        let Some(id) = current else { break stop.is_none() };
        if Some(id.as_str()) == stop {
            break true;
        }
        // An entry the listing doesn't have: the walk can't go on.
        let Some(entry) = tree.by_id.get(&id) else { break stop.is_none() };
        if !seen.insert(id.clone()) {
            return None;
        }
        if entry.user {
            user_entries.push(id.clone());
        }
        current = entry.parent.clone();
    };
    Some(Walk { user_entries, reached_stop })
}

/// The first user entry after `through` on the active branch: where a copy
/// that keeps the conversation through that turn is cut.
pub fn next_user_entry(tree: &Entries, through: &str) -> Option<String> {
    let branch = walk(tree, None)?.user_entries;
    let at = branch.iter().position(|id| id == through)?;
    at.checked_sub(1).map(|i| branch[i].clone())
}

// ---------------------------------------------------------------------------
// Short-lived processes: forks and rollbacks of a session file
// ---------------------------------------------------------------------------

async fn short_lived(cwd: &str, fork: Option<&str>) -> Result<Arc<PiRpc>, String> {
    let exe = crate::providers::executable(crate::pi_rpc::PI_TOOL)?;
    let launch = Launch { short_lived: true, no_extensions: true, no_tools: true, fork: fork.map(str::to_owned), user_args: crate::pi_rpc::user_launch_args()?, ..Launch::default() };
    let (rpc, mut events) = PiRpc::spawn(&exe, &launch, cwd, &crate::pi_rpc::environment("approval-required", None), |_, _| {}).map_err(|e| format!("could not start pi: {e}"))?;
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    Ok(rpc)
}

fn session_file(state: &Value) -> Result<String, String> {
    state["sessionFile"].as_str().filter(|f| !f.is_empty()).map(str::to_owned).ok_or_else(|| "Pi did not report a session file.".to_owned())
}

fn not_cancelled(data: &Value, what: &str) -> Result<(), String> {
    if data["cancelled"] == true { Err(format!("A pi extension cancelled the {what}.")) } else { Ok(()) }
}

/// A new session file copied from `source`, keeping its conversation through
/// the turn whose user entry is `through` (all of it without one).
pub async fn fork_file(source: &str, cwd: &str, through: Option<&str>) -> Result<String, String> {
    let rpc = short_lived(cwd, Some(source)).await?;
    let result = async {
        if let Some(at) = through {
            let tree = entries(&rpc.request(json!({ "type": "get_entries" }), DEFAULT_TIMEOUT).await?).ok_or("Pi's session tree could not be read.")?;
            if let Some(cut) = next_user_entry(&tree, at) {
                not_cancelled(&rpc.request(json!({ "type": "fork", "entryId": cut }), LIFECYCLE_TIMEOUT).await?, "fork")?;
            }
        }
        session_file(&rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await?)
    }
    .await;
    rpc.terminate().await;
    result
}

/// A new session file with `source`'s conversation up to (not including) the
/// user entry `before`: a rollback of a thread with no pi running.
pub async fn rollback_file(source: &str, cwd: &str, before: &str) -> Result<String, String> {
    let rpc = short_lived(cwd, None).await?;
    let result = async {
        not_cancelled(&rpc.request(json!({ "type": "switch_session", "sessionPath": source }), LIFECYCLE_TIMEOUT).await?, "session switch")?;
        not_cancelled(&rpc.request(json!({ "type": "fork", "entryId": before }), LIFECYCLE_TIMEOUT).await?, "rollback")?;
        session_file(&rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await?)
    }
    .await;
    rpc.terminate().await;
    result
}

// ---------------------------------------------------------------------------
// A thread's pi
// ---------------------------------------------------------------------------

pub struct PiSession {
    rpc: Arc<PiRpc>,
    events: Mutex<mpsc::UnboundedReceiver<PiEvent>>,
    pub runtime_mode: String,
    pub cwd: String,
    session_file: std::sync::Mutex<String>,
    /// pi's own model and thinking level when it started: what `default` restores.
    baseline: (Value, Option<String>),
    /// The model this process runs now (`provider`, `id`, `contextWindow`).
    current_model: std::sync::Mutex<Value>,
    models: Vec<Value>,
    /// The session tree's leaf after the last turn: the next turn's entries follow it.
    last_leaf: std::sync::Mutex<Option<String>>,
    /// Dialogs answered "for this session" (title and message).
    approvals: std::sync::Mutex<HashSet<String>>,
    idle: std::sync::Mutex<Option<(oneshot::Sender<()>, tokio::task::JoinHandle<()>)>>,
    broken: AtomicBool,
}

impl PiSession {
    /// Starts pi for a thread: on its session file when it has one, on a copy
    /// of the fork source for a forked thread's first turn, else on a new session.
    pub async fn start(input: &TurnInput, native_thread_id: Option<String>, userdata: &Path, log_dir: PathBuf) -> Result<Arc<Self>, String> {
        let exe = crate::providers::executable(crate::pi_rpc::PI_TOOL)?;
        let user_args = crate::pi_rpc::user_launch_args()?;
        let extension = write_extension(userdata)?;
        let native = match (native_thread_id, &input.fork) {
            // pi writes a session file once it holds a reply: a rollback to
            // before the first message names one not written yet, which is
            // an empty conversation, so a new session.
            (Some(file), _) if !Path::new(&file).exists() => {
                tracing::info!(%file, "pi's session file was never written; starting a new session");
                None
            }
            (Some(file), _) => Some(file),
            (None, Some((source, through))) => Some(fork_file(source, &input.cwd, through.as_deref().filter(|t| !t.starts_with(PENDING_REF))).await?),
            (None, None) => None,
        };
        let mut env = crate::pi_rpc::environment(&input.runtime_mode, crate::mcp::session_for(&input.thread_id));
        env.push((EXTENSION_PATH_ENV.into(), Some(extension.to_string_lossy().into_owned())));
        let launch = Launch { extension: Some(extension), user_args, ..Launch::default() };
        let log = crate::logging::protocol_log(log_dir.join(format!("pi-{}.ndjson", crate::util::new_uuid())));
        let (rpc, events) = PiRpc::spawn(&exe, &launch, &input.cwd, &env, log).map_err(|e| format!("could not start pi: {e}"))?;
        let opened = async {
            if let Some(file) = &native {
                not_cancelled(&rpc.request(json!({ "type": "switch_session", "sessionPath": file }), LIFECYCLE_TIMEOUT).await?, "session switch")?;
            }
            let state = rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await?;
            let file = session_file(&state)?;
            let models = rpc.request(json!({ "type": "get_available_models" }), DEFAULT_TIMEOUT).await.ok().and_then(|d| d["models"].as_array().cloned()).unwrap_or_default();
            let leaf = rpc.request(json!({ "type": "get_entries" }), DEFAULT_TIMEOUT).await.ok().and_then(|d| d["leafId"].as_str().map(str::to_owned));
            Ok::<_, String>((state, file, models, leaf))
        }
        .await;
        let (state, file, models, leaf) = match opened {
            Ok(o) => o,
            Err(e) => {
                rpc.terminate().await;
                return Err(e);
            }
        };
        Ok(Arc::new(PiSession {
            rpc,
            events: Mutex::new(events),
            runtime_mode: input.runtime_mode.clone(),
            cwd: input.cwd.clone(),
            session_file: std::sync::Mutex::new(file),
            baseline: (state["model"].clone(), state["thinkingLevel"].as_str().map(str::to_owned)),
            current_model: std::sync::Mutex::new(state["model"].clone()),
            models,
            last_leaf: std::sync::Mutex::new(leaf),
            approvals: Default::default(),
            idle: Default::default(),
            broken: AtomicBool::new(false),
        }))
    }

    pub fn session_file(&self) -> String {
        crate::util::locked(&self.session_file).clone()
    }

    /// The process can take another turn (in this runtime mode).
    pub fn usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst) && !self.rpc.exited()
    }

    pub async fn kill(&self) {
        self.stop_idle_drain().await;
        self.rpc.terminate().await;
    }

    /// Between turns pi may still start work (an extension waking it): that
    /// would run tools nobody sees, so it is stopped; a dialog is cancelled.
    pub fn start_idle_drain(self: &Arc<Self>) {
        let (stop, mut stopped) = oneshot::channel();
        let me = self.clone();
        let task = tokio::spawn(async move {
            let mut events = me.events.lock().await;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    ev = events.recv() => match ev {
                        None | Some(PiEvent::Exited(_)) => {
                            me.broken.store(true, Ordering::SeqCst);
                            break;
                        }
                        Some(PiEvent::Record(r)) => me.idle_record(&r).await,
                    },
                }
            }
        });
        *crate::util::locked(&self.idle) = Some((stop, task));
    }

    async fn idle_record(&self, r: &Value) {
        match r["type"].as_str() {
            Some("agent_start") => {
                tracing::warn!(session = %self.session_file(), "pi started work outside a turn; stopping it");
                let _ = self.rpc.request(json!({ "type": "abort" }), SHORT_TIMEOUT).await;
            }
            Some("extension_ui_request") if matches!(r["method"].as_str(), Some("select" | "confirm" | "input" | "editor")) => {
                tracing::info!(title = %r["title"], "{IDLE_DECLINED}");
                if let Some(id) = r["id"].as_str() {
                    let _ = self.rpc.send(&json!({ "type": "extension_ui_response", "id": id, "cancelled": true })).await;
                }
            }
            _ => {}
        }
    }

    async fn stop_idle_drain(&self) {
        let running = crate::util::locked(&self.idle).take();
        if let Some((stop, task)) = running {
            let _ = stop.send(());
            let _ = task.await;
        }
    }

    /// Rolls the live session back to before the user entry `before`: pi
    /// switches to a new session file, returned.
    pub async fn rollback(&self, before: &str) -> Result<String, String> {
        self.stop_idle_drain().await;
        let result = async {
            not_cancelled(&self.rpc.request(json!({ "type": "fork", "entryId": before }), LIFECYCLE_TIMEOUT).await?, "rollback")?;
            let state = self.rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await?;
            let file = session_file(&state)?;
            *crate::util::locked(&self.session_file) = file.clone();
            let leaf = self.rpc.request(json!({ "type": "get_entries" }), DEFAULT_TIMEOUT).await.ok().and_then(|d| d["leafId"].as_str().map(str::to_owned));
            *crate::util::locked(&self.last_leaf) = leaf;
            Ok::<_, String>(file)
        }
        .await;
        if result.is_err() {
            self.broken.store(true, Ordering::SeqCst);
        }
        result
    }

    /// The model, thinking level and service tier the selection asks for.
    async fn apply_selection(&self, input: &TurnInput) -> Result<(), String> {
        let selection = &input.model_selection;
        let wanted = if selection.model == crate::providers::PI_DEFAULT_MODEL || selection.model.is_empty() {
            self.baseline.0["provider"].as_str().zip(self.baseline.0["id"].as_str()).map(|(p, i)| (p.to_owned(), i.to_owned()))
        } else {
            Some(crate::pi_rpc::resolve_model_slug(&selection.model, &self.models).ok_or_else(|| format!("Pi has no model `{}`.", selection.model))?)
        };
        let current = crate::util::locked(&self.current_model).clone();
        let switched = match &wanted {
            Some((p, i)) if current["provider"].as_str() != Some(p.as_str()) || current["id"].as_str() != Some(i.as_str()) => {
                let model = self.rpc.request(json!({ "type": "set_model", "provider": p, "modelId": i }), DEFAULT_TIMEOUT).await?;
                *crate::util::locked(&self.current_model) = if model.is_object() { model } else { json!({ "provider": p, "id": i }) };
                true
            }
            _ => false,
        };
        // A model switch resets the level, so it is set again; `default` keeps pi's own.
        let level = super::claude::chosen(selection, &["thinking"]).map(str::to_owned).or_else(|| (switched || selection.model == crate::providers::PI_DEFAULT_MODEL).then(|| self.baseline.1.clone()).flatten());
        if let Some(level) = level {
            self.rpc.request(json!({ "type": "set_thinking_level", "level": level }), DEFAULT_TIMEOUT).await?;
        }
        if let Some(tier) = super::claude::chosen(selection, &["serviceTier"]) {
            let tier = if tier == crate::providers::PI_STANDARD_SERVICE_TIER { "" } else { tier };
            let data = self.rpc.request(json!({ "type": "prompt", "message": format!("/domovoi-service-tier {tier}").trim_end() }), DEFAULT_TIMEOUT).await?;
            if !matches!(data["disposition"].as_str(), None | Some("handled")) {
                return Err("Pi ran the service tier command as a message: the Domovoi extension did not load.".into());
            }
        }
        Ok(())
    }

    fn context_window(&self) -> Option<u64> {
        crate::util::locked(&self.current_model)["contextWindow"].as_u64()
    }

    /// Runs one turn. Returns the turn and whether the process can take another.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> (Turn, bool) {
        self.stop_idle_drain().await;
        let file = self.session_file();
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &input.model_selection.model, None);
        turn.thread_status(ProviderThreadStatus::Active, Some(&file));
        let mut events = self.events.lock().await;
        // What came since the last turn is not this one's.
        while let Ok(ev) = events.try_recv() {
            match ev {
                PiEvent::Exited(code) => {
                    turn.complete(TurnEnd::Failed, Some(&exited(code)));
                    return (turn, false);
                }
                PiEvent::Record(r) => self.idle_record(&r).await,
            }
        }
        if let Err(e) = self.apply_selection(&input).await {
            turn.complete(TurnEnd::Failed, Some(&e));
            return (turn, self.usable());
        }
        if let Some(title) = input.title.as_deref().filter(|t| !t.trim().is_empty()) {
            let _ = self.rpc.request(json!({ "type": "set_session_name", "name": title }), DEFAULT_TIMEOUT).await;
        }
        turn.turn_started(&format!("{PENDING_REF}{}", crate::util::new_uuid()));

        let mut state = TurnState::default();
        // `/compact` runs pi's compaction instead of a prompt; its answer ends the turn.
        #[expect(clippy::disallowed_methods, reason = "one answer at most")]
        let (compact_tx, mut compact_rx) = mpsc::unbounded_channel::<Result<Value, String>>();
        if let Some(instructions) = parse_compact(&input.text).filter(|_| input.images.is_empty()) {
            state.compacting = true;
            let rpc = self.rpc.clone();
            tokio::spawn(async move {
                let mut cmd = json!({ "type": "compact" });
                if let Some(i) = instructions {
                    cmd["customInstructions"] = json!(i);
                }
                // A compaction takes as long as a model call over the whole conversation.
                let _ = compact_tx.send(rpc.request(cmd, Duration::from_secs(30 * 60)).await.map_err(String::from));
            });
        } else {
            match self.prompt(&input.text, &input.images).await {
                Ok(d) if d["disposition"] == "handled" => {
                    // An extension command ran: no agent work follows.
                    self.finish(&mut turn, &state).await;
                    turn.complete(TurnEnd::Completed, None);
                    return (turn, true);
                }
                Ok(_) => {}
                Err(e) => {
                    turn.complete(TurnEnd::Failed, Some(&format!("Pi refused the message: {e}")));
                    return (turn, self.usable());
                }
            }
        }

        let mut settle_retry: Option<(tokio::time::Instant, u32)> = None;
        let mut usable = true;
        let end = loop {
            let deadline = state.dialogs.values().filter_map(|d| d.expires).min();
            tokio::select! {
                _ = tokio::time::sleep_until(settle_retry.map_or_else(tokio::time::Instant::now, |(at, _)| at)), if settle_retry.is_some() => {
                    let tries = settle_retry.take().map_or(0, |(_, n)| n);
                    match self.settled().await {
                        Some(true) => break state.end(),
                        Some(false) if tries + 1 < SETTLE_PROBES => settle_retry = Some((tokio::time::Instant::now() + SETTLE_RETRY, tries + 1)),
                        _ => {
                            usable = false;
                            state.failure.get_or_insert_with(|| "Pi did not settle after its turn.".into());
                            break TurnEnd::Failed;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)), if deadline.is_some() => {
                    let now = tokio::time::Instant::now();
                    let expired: Vec<String> = state.dialogs.iter().filter(|(_, d)| d.expires.is_some_and(|e| e <= now)).map(|(id, _)| id.clone()).collect();
                    for id in expired {
                        state.dialogs.remove(&id);
                        // pi resolved it itself when its timeout passed.
                        turn.resolve_request(&id, "cancel", None);
                    }
                }
                Some(answer) = compact_rx.recv() => {
                    state.compacting = false;
                    match answer {
                        Ok(_) => {}
                        Err(e) if state.interrupted => tracing::info!("compaction stopped: {e}"),
                        Err(e) => {
                            state.failure.get_or_insert(format!("Compaction failed: {e}"));
                        }
                    }
                    if self.settled().await != Some(false) {
                        break state.end();
                    }
                    settle_retry = Some((tokio::time::Instant::now() + SETTLE_RETRY, 0));
                }
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, answers } => self.respond(&mut turn, &mut state, &request_id, &decision, answers.as_ref()).await,
                    Control::Interrupt => {
                        state.interrupted = true;
                        if state.compacting {
                            // pi can't abort a compaction from RPC: stop the process.
                            self.rpc.terminate().await;
                            usable = false;
                            break TurnEnd::Interrupted;
                        }
                        if let Ok(q) = self.rpc.request(json!({ "type": "clear_queue" }), SHORT_TIMEOUT).await {
                            let dropped: Vec<&str> = q["steering"].as_array().into_iter().chain(q["followUp"].as_array()).flatten().filter_map(Value::as_str).collect();
                            if !dropped.is_empty() {
                                turn.item(&format!("cleared:{}", crate::util::new_uuid()), Work::Notice { message: format!("Not sent to Pi (stopped first): {}", dropped.join(" / ")) }, TurnItemStatus::Completed);
                            }
                        }
                        if self.rpc.request(json!({ "type": "abort" }), SHORT_TIMEOUT).await.is_err() {
                            self.rpc.terminate().await;
                            usable = false;
                            break TurnEnd::Interrupted;
                        }
                    }
                    Control::Steer { text, images, reply } => {
                        let sent = self.prompt(&text, &images).await.map(|_| ());
                        let _ = reply.send(sent.map_err(|e| e.to_string()));
                    }
                    Control::StopSubagent { reply, .. } => {
                        let _ = reply.send(Err("Pi subagents can't be stopped one by one.".into()));
                    }
                },
                ev = events.recv() => match ev {
                    None | Some(PiEvent::Exited(_)) if state.interrupted => {
                        usable = false;
                        break TurnEnd::Interrupted;
                    }
                    None => {
                        usable = false;
                        state.failure = Some(exited(None));
                        break TurnEnd::Failed;
                    }
                    Some(PiEvent::Exited(code)) => {
                        usable = false;
                        state.failure = Some(exited(code));
                        break TurnEnd::Failed;
                    }
                    Some(PiEvent::Record(r)) => {
                        if r["type"] == "agent_settled" {
                            match self.settled().await {
                                Some(true) => break state.end(),
                                _ => settle_retry = Some((tokio::time::Instant::now() + SETTLE_RETRY, 0)),
                            }
                        } else {
                            self.record(&mut turn, &mut state, &r).await;
                        }
                    }
                },
            }
        };
        drop(events);
        // Dialogs still open can't be answered any more.
        for id in state.dialogs.keys().cloned().collect::<Vec<_>>() {
            turn.resolve_request(&id, "cancel", None);
        }
        if usable {
            self.finish(&mut turn, &state).await;
        } else {
            self.broken.store(true, Ordering::SeqCst);
        }
        let error = state.failure.clone().filter(|_| end == TurnEnd::Failed);
        turn.complete(end, error.as_deref());
        (turn, usable)
    }

    async fn prompt(&self, text: &str, images: &[(String, PathBuf)]) -> Result<Value, crate::pi_rpc::RpcError> {
        use base64::Engine as _;
        let images: Vec<Value> = images
            .iter()
            .filter_map(|(mime, path)| std::fs::read(path).ok().map(|b| json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(b), "mimeType": mime })))
            .collect();
        let mut cmd = json!({ "type": "prompt", "message": text, "streamingBehavior": "steer" });
        if !images.is_empty() {
            cmd["images"] = Value::Array(images);
        }
        // A prompt that races a compaction pi started by itself is sent again once it ends.
        for _ in 0..30 {
            match self.rpc.request(cmd.clone(), DEFAULT_TIMEOUT).await {
                Err(crate::pi_rpc::RpcError::Refused(e)) if e.contains(COMPACTING_REJECTION) => tokio::time::sleep(Duration::from_secs(1)).await,
                other => return other,
            }
        }
        Err(crate::pi_rpc::RpcError::Refused(COMPACTING_REJECTION.into()))
    }

    /// Whether pi is done: not streaming, compacting, or holding queued
    /// messages. `None` if it didn't answer.
    async fn settled(&self) -> Option<bool> {
        let s = self.rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await.ok()?;
        Some(s["isStreaming"] != true && s["isCompacting"] != true && s["pendingMessageCount"].as_u64().unwrap_or(0) == 0)
    }

    /// After a turn: its user entry as its ref (what rollback and fork cut
    /// at), and the context use pi reports for the session.
    async fn finish(&self, turn: &mut Turn, state: &TurnState) {
        let previous = crate::util::locked(&self.last_leaf).clone();
        let mut since = json!({ "type": "get_entries" });
        if let Some(leaf) = &previous {
            since["since"] = json!(leaf);
        }
        let tree = self.rpc.request(since, DEFAULT_TIMEOUT).await.ok().and_then(|d| entries(&d));
        if let Some(tree) = tree {
            match walk(&tree, previous.as_deref()) {
                // The walk reached the previous leaf: everything after it is this turn's.
                Some(w) if w.reached_stop => {
                    if let Some(first) = w.user_entries.last() {
                        turn.set_turn_cursor(first);
                    }
                }
                // Rewound or replaced (an extension navigated the tree): no ref is safe to cut at.
                _ => tracing::info!("pi's session tree moved outside the turn; no turn ref recorded"),
            }
            *crate::util::locked(&self.last_leaf) = tree.leaf.clone();
        }
        if let Ok(stats) = self.rpc.request(json!({ "type": "get_session_stats" }), SHORT_TIMEOUT).await
            && let Some(used) = stats["contextUsage"]["tokens"].as_u64()
        {
            let t = &stats["tokens"];
            turn.token_usage(WindowUse {
                used_tokens: used,
                max_tokens: stats["contextUsage"]["contextWindow"].as_u64().or_else(|| self.context_window()),
                input_tokens: t["input"].as_u64().unwrap_or(0) + t["cacheRead"].as_u64().unwrap_or(0) + t["cacheWrite"].as_u64().unwrap_or(0),
                cached_input_tokens: t["cacheRead"].as_u64().unwrap_or(0),
                output_tokens: t["output"].as_u64().unwrap_or(0),
                reasoning_output_tokens: 0,
            });
        } else if let Some(u) = &state.last_usage {
            turn.token_usage(u.clone());
        }
    }

    async fn respond(&self, turn: &mut Turn, state: &mut TurnState, request_id: &str, decision: &str, answers: Option<&Answers>) {
        let Some(dialog) = state.dialogs.remove(request_id) else { return };
        let mut reply = json!({ "type": "extension_ui_response", "id": dialog.native_id });
        if dialog.method == "confirm" {
            match decision {
                d if decision_allows(d) => reply["confirmed"] = json!(true),
                "decline" => reply["confirmed"] = json!(false),
                _ => reply["cancelled"] = json!(true),
            }
            if decision == "acceptForSession" {
                crate::util::locked(&self.approvals).insert(dialog.key.clone());
            }
        } else {
            match answers.and_then(|a| a.get(&dialog.native_id)).and_then(answer_text) {
                Some(text) if decision_allows(decision) || decision.is_empty() => reply["value"] = json!(text),
                _ => reply["cancelled"] = json!(true),
            }
        }
        let _ = self.rpc.send(&reply).await;
        turn.resolve_request(request_id, decision, answers);
    }

    /// One pi event of the running turn, recorded.
    async fn record(&self, turn: &mut Turn, state: &mut TurnState, r: &Value) {
        let base = turn.run_id();
        match r["type"].as_str().unwrap_or_default() {
            "message_start" if r["message"]["role"] == "assistant" => state.message += 1,
            "message_update" => {
                let e = &r["assistantMessageEvent"];
                let index = e["contentIndex"].as_u64().unwrap_or(0);
                let id = format!("{base}:m{}:c{index}", state.message);
                match e["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = state.texts.entry(id.clone()).or_default();
                        text.push_str(e["delta"].as_str().unwrap_or_default());
                        let text = text.clone();
                        turn.assistant(&id, &text, true, None);
                    }
                    "text_end" => {
                        let text = e["content"].as_str().map(str::to_owned).or_else(|| state.texts.remove(&id)).unwrap_or_default();
                        state.texts.remove(&id);
                        turn.assistant(&id, &text, false, None);
                    }
                    "thinking_delta" => {
                        let text = state.texts.entry(id.clone()).or_default();
                        text.push_str(e["delta"].as_str().unwrap_or_default());
                        let text = text.clone();
                        let len = text.len();
                        turn.item_streaming(&id, Work::Reasoning { text, streaming: true }, len);
                    }
                    "thinking_end" => {
                        let text = e["content"].as_str().map(str::to_owned).or_else(|| state.texts.remove(&id)).unwrap_or_default();
                        state.texts.remove(&id);
                        turn.item(&id, Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
                    }
                    _ => {}
                }
            }
            "message_end" if r["message"]["role"] == "assistant" => {
                let m = &r["message"];
                match m["stopReason"].as_str() {
                    Some("error") => state.failure = Some(m["errorMessage"].as_str().unwrap_or("Pi's model call failed.").to_owned()),
                    Some("aborted") => state.aborted = true,
                    _ => {}
                }
                let u = &m["usage"];
                if let Some(total) = u["totalTokens"].as_u64().filter(|t| *t > 0) {
                    let n = |k: &str| u[k].as_u64().unwrap_or(0);
                    let usage = WindowUse {
                        used_tokens: total,
                        max_tokens: self.context_window(),
                        input_tokens: n("input") + n("cacheRead") + n("cacheWrite"),
                        cached_input_tokens: n("cacheRead"),
                        output_tokens: n("output"),
                        reasoning_output_tokens: 0,
                    };
                    turn.token_usage(usage.clone());
                    state.last_usage = Some(usage);
                }
            }
            ty @ ("tool_execution_start" | "tool_execution_update" | "tool_execution_end") => {
                let Some(id) = r["toolCallId"].as_str() else { return };
                let name = r["toolName"].as_str().unwrap_or("tool").to_owned();
                if ty == "tool_execution_start" {
                    state.tool_args.insert(id.to_owned(), r["args"].clone());
                }
                let args = if r["args"].is_null() { state.tool_args.get(id).cloned().unwrap_or(Value::Null) } else { r["args"].clone() };
                let ended = ty == "tool_execution_end";
                let result = if ended { &r["result"] } else { &r["partialResult"] };
                let status = match (ended, r["isError"] == true) {
                    (false, _) => TurnItemStatus::Running,
                    (true, true) if state.interrupted => TurnItemStatus::Interrupted,
                    (true, true) => TurnItemStatus::Failed,
                    (true, false) => TurnItemStatus::Completed,
                };
                let work = tool_work(&name, &args, result, ended, r["isError"] == true);
                if ty == "tool_execution_update" {
                    let len = content_text(&result["content"]).len();
                    turn.item_streaming(id, work, len);
                } else {
                    turn.item(id, work, status);
                }
            }
            "compaction_start" => {
                state.compaction += 1;
                state.compacting = true;
                turn.item(&format!("{base}:compaction:{}", state.compaction), Work::Compaction { before_token_count: None }, TurnItemStatus::Running);
            }
            "compaction_end" => {
                state.compacting = false;
                let id = format!("{base}:compaction:{}", state.compaction);
                let before = r["result"]["tokensBefore"].as_u64();
                let status = if r["aborted"] == true {
                    TurnItemStatus::Cancelled
                } else if r["errorMessage"].as_str().is_some() && r["willRetry"] != true {
                    state.failure.get_or_insert(format!("Compaction failed: {}", r["errorMessage"].as_str().unwrap_or_default()));
                    TurnItemStatus::Failed
                } else {
                    TurnItemStatus::Completed
                };
                turn.item(&id, Work::Compaction { before_token_count: before }, status);
            }
            "auto_retry_start" => {
                let message = format!(
                    "Retrying (attempt {} of {}) in {}s: {}",
                    r["attempt"].as_u64().unwrap_or(1),
                    r["maxAttempts"].as_u64().unwrap_or(1),
                    r["delayMs"].as_u64().unwrap_or(0).div_ceil(1000),
                    r["errorMessage"].as_str().unwrap_or("the model call failed")
                );
                turn.item(&format!("{base}:retry:{}", r["attempt"]), Work::Notice { message }, TurnItemStatus::Completed);
            }
            "auto_retry_end" if r["success"] == false => {
                state.failure = Some(r["finalError"].as_str().unwrap_or("Pi gave up retrying the model call.").to_owned());
            }
            "extension_error" => {
                let message = format!("Pi extension {} failed ({}): {}", extension_name(r["extensionPath"].as_str()), r["event"].as_str().unwrap_or("event"), r["error"].as_str().unwrap_or("unknown error"));
                turn.item(&format!("{base}:extension-error:{}", crate::util::new_uuid()), Work::Notice { message }, TurnItemStatus::Failed);
            }
            "extension_ui_request" => self.dialog(turn, state, r).await,
            _ => {}
        }
    }

    async fn dialog(&self, turn: &mut Turn, state: &mut TurnState, r: &Value) {
        let (Some(method), Some(native_id)) = (r["method"].as_str(), r["id"].as_str()) else { return };
        let title = r["title"].as_str().unwrap_or(method).to_owned();
        let message = r["message"].as_str().unwrap_or_default().to_owned();
        match method {
            "notify" => {
                if !message.is_empty() {
                    turn.item(&format!("notify:{native_id}"), Work::Notice { message }, TurnItemStatus::Completed);
                }
                return;
            }
            "select" | "confirm" | "input" | "editor" => {}
            // setStatus, setWidget, setTitle, set_editor_text: a terminal's furniture.
            _ => return,
        }
        let key = format!("{}:{title}{message}", title.len());
        if method == "confirm" && crate::util::locked(&self.approvals).contains(&key) {
            let _ = self.rpc.send(&json!({ "type": "extension_ui_response", "id": native_id, "confirmed": true })).await;
            return;
        }
        let request_id = if method == "confirm" {
            let prompt = if message.is_empty() { title.clone() } else { message };
            turn.open_request(native_id, native_id, approval_kind(&title), &prompt)
        } else {
            turn.open_user_input(native_id, native_id, vec![question(native_id, method, &title, r)])
        };
        // pi resolves the dialog itself when its timeout passes (0: never).
        let expires = r["timeout"].as_u64().filter(|t| *t > 0).map(|t| tokio::time::Instant::now() + Duration::from_millis(t));
        state.dialogs.insert(request_id, Dialog { native_id: native_id.to_owned(), method: method.to_owned(), key, expires });
    }
}

fn exited(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("Pi exited (code {c})."),
        None => "Pi exited.".into(),
    }
}

#[derive(Default)]
struct TurnState {
    /// Assistant messages so far (item ids are per message and content block).
    message: u64,
    texts: HashMap<String, String>,
    tool_args: HashMap<String, Value>,
    dialogs: HashMap<String, Dialog>,
    compaction: u64,
    compacting: bool,
    interrupted: bool,
    /// pi reported the model call aborted.
    aborted: bool,
    failure: Option<String>,
    last_usage: Option<WindowUse>,
}

impl TurnState {
    fn end(&self) -> TurnEnd {
        if self.interrupted || self.aborted {
            TurnEnd::Interrupted
        } else if self.failure.is_some() {
            TurnEnd::Failed
        } else {
            TurnEnd::Completed
        }
    }
}

struct Dialog {
    native_id: String,
    method: String,
    key: String,
    expires: Option<tokio::time::Instant>,
}

/// The extension asks `Allow <tool>?`: edits and writes as file changes.
pub fn approval_kind(title: &str) -> ProviderRequestKind {
    match title.strip_prefix("Allow ").and_then(|t| t.strip_suffix('?')) {
        Some("edit" | "write") => ProviderRequestKind::FileChange,
        _ => ProviderRequestKind::Command,
    }
}

/// A `select`, `input` or `editor` dialog as a question keyed by its native id.
pub fn question(native_id: &str, method: &str, title: &str, r: &Value) -> UserInputQuestion {
    let option = |label: &str, description: &str, value: &str| QuestionOption { label: label.to_owned(), description: description.to_owned(), value: Some(value.to_owned()), preview: None };
    let options = match method {
        "select" => r["options"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|o| if o.is_empty() { option("Empty value", "Empty value", "") } else { option(o, o, o) }).collect(),
        "editor" => vec![],
        _ => vec![option("Submit empty value", "Send an empty string to the extension.", "")],
    };
    // Select and editor dialogs carry only a title: show it once, as the question.
    let detail = [r["message"].as_str(), r["placeholder"].as_str()].into_iter().flatten().find(|t| !t.trim().is_empty());
    let title = if title.trim().is_empty() { "Question" } else { title };
    UserInputQuestion {
        id: native_id.to_owned(),
        header: if detail.is_some() { title.to_owned() } else { "Question".to_owned() },
        question: detail.unwrap_or(title).to_owned(),
        options,
        multi_select: None,
        allow_custom_answer: (method != "select").then_some(true),
        initial_answer: (method == "editor").then(|| r["prefill"].as_str().unwrap_or_default().to_owned()),
        required: None,
    }
}

/// An answer as the one string a pi dialog takes (the first of several).
fn answer_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(Value::as_str).map(str::to_owned),
        _ => None,
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

fn extension_name(path: Option<&str>) -> String {
    let Some(path) = path else { return "Pi extension".into() };
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let file = parts.last().map(|f| f.rsplit_once('.').map_or(*f, |(n, _)| n)).unwrap_or_default();
    // A folder extension loads from its index file: the folder names it.
    let name = if file == "index" { parts.len().checked_sub(2).map_or(file, |i| parts[i]) } else { file };
    if name.is_empty() { "Pi extension".into() } else { name.to_owned() }
}

/// A pi tool call as a turn item: shell commands, file edits, or a generic tool.
pub fn tool_work(name: &str, args: &Value, result: &Value, ended: bool, is_error: bool) -> Work {
    let text = content_text(&result["content"]);
    match name {
        "bash" | "powershell" => {
            let exit_code = result["structuredContent"]["exit_code"].as_i64().or_else(|| result["details"]["exitCode"].as_i64()).or_else(|| ended.then_some(i64::from(is_error)));
            Work::Command { input: args["command"].as_str().unwrap_or_default().to_owned(), output: (!text.is_empty()).then_some(text), exit_code }
        }
        "edit" | "write" if args["path"].as_str().or(args["file_path"].as_str()).is_some() => {
            let path = args["path"].as_str().or(args["file_path"].as_str()).unwrap_or_default();
            let file_name = Path::new(path).file_name().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
            let operation = if name == "write" { "add" } else { "update" };
            let patch = result["details"]["patch"].as_str().map(str::to_owned).or_else(|| (is_error && !text.trim().is_empty()).then(|| text.clone()));
            let texts = if name == "write" {
                args["content"].as_str().map(|c| (String::new(), c.to_owned()))
            } else {
                args["oldText"].as_str().zip(args["newText"].as_str()).map(|(o, n)| (o.to_owned(), n.to_owned()))
            };
            Work::FileChange {
                file_name,
                changes: vec![FileChangeDetail { operation: operation.into(), path: path.to_owned(), old_path: None, file_type: None, mime_type: None }],
                texts,
                patch,
                error: None,
            }
        }
        _ => {
            let output = tool_output(result, name);
            Work::Tool { name: name.to_owned(), input: if args.is_null() { json!({}) } else { args.clone() }, output, viewed_image_path: None }
        }
    }
}

/// A tool's result as shown: its text when that's all it is, else its content
/// blocks (and structured content, when small enough to keep).
fn tool_output(result: &Value, name: &str) -> Option<Value> {
    let content = match &result["content"] {
        Value::Array(c) => c.clone(),
        Value::String(s) => vec![json!({ "type": "text", "text": s })],
        _ => vec![],
    };
    let mut structured = result["structuredContent"].clone();
    let mcp = name.starts_with("mcp__") && result["details"]["server"].is_string() && structured["content"].is_array();
    if mcp {
        structured = structured["structuredContent"].clone();
    }
    if structured.as_str().is_some_and(|s| s == content_text(&Value::Array(content.clone()))) {
        structured = Value::Null;
    }
    if !structured.is_null() && serde_json::to_vec(&structured).map_or(0, |b| b.len()) > 64 * 1024 {
        let mut content = content;
        content.push(json!({ "type": "text", "text": "Structured output omitted because it exceeds the stored result limit." }));
        return Some(json!({ "content": content }));
    }
    if !mcp && structured.is_null() && content.iter().all(|b| b["type"] == "text") {
        let text = content_text(&Value::Array(content));
        return (!text.is_empty()).then_some(Value::String(text));
    }
    let mut out = json!({ "content": content });
    if !structured.is_null() {
        out["structuredContent"] = structured;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries_json: Value, leaf: &str) -> Entries {
        entries(&json!({ "entries": entries_json, "leafId": leaf })).unwrap()
    }

    fn e(id: &str, parent: Option<&str>, user: bool) -> Value {
        json!({ "id": id, "parentId": parent, "type": if user { "message" } else { "model_change" }, "message": { "role": if user { "user" } else { "assistant" } } })
    }

    #[test]
    fn a_turn_walk_stops_at_the_previous_leaf() {
        // u1 -> a1 | u2 -> a2 -> u3(steer) -> a3
        let t = tree(json!([e("u1", None, true), e("a1", Some("u1"), false), e("u2", Some("a1"), true), e("a2", Some("u2"), false), e("u3", Some("a2"), true), e("a3", Some("u3"), false)]), "a3");
        let w = walk(&t, Some("a1")).unwrap();
        assert!(w.reached_stop);
        assert_eq!(w.user_entries, ["u3", "u2"], "newest first: the turn's own entry is the last");
        assert!(!walk(&t, Some("gone")).unwrap().reached_stop, "a rewound or replaced tree does not reach the old leaf");
        assert_eq!(next_user_entry(&t, "u1").as_deref(), Some("u2"));
        assert_eq!(next_user_entry(&t, "u3"), None, "through the latest turn: nothing to cut");
    }

    #[test]
    fn a_tree_that_is_not_one_is_refused() {
        assert!(entries(&json!({ "entries": [e("x", None, true), e("x", None, false)] })).is_none(), "an id twice");
        assert!(entries(&json!({ "entries": [{ "id": "x", "parentId": 3 }] })).is_none());
        let cyclic = tree(json!([e("a", Some("b"), true), e("b", Some("a"), false)]), "a");
        assert!(walk(&cyclic, None).is_none());
    }

    #[test]
    fn compact_is_a_command_only_by_itself() {
        assert_eq!(parse_compact(" /compact "), Some(None));
        assert_eq!(parse_compact("/compact keep the API notes"), Some(Some("keep the API notes".into())));
        assert_eq!(parse_compact("/compacted"), None);
        assert_eq!(parse_compact("please /compact"), None);
    }

    #[test]
    fn dialogs_become_approvals_and_questions() {
        assert_eq!(approval_kind("Allow edit?"), ProviderRequestKind::FileChange);
        assert_eq!(approval_kind("Allow bash?"), ProviderRequestKind::Command);
        let q = question("d1", "select", "Pick", &json!({ "options": ["a", ""] }));
        assert_eq!((q.header.as_str(), q.question.as_str()), ("Question", "Pick"));
        assert_eq!(q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["a", "Empty value"]);
        let q = question("d2", "editor", "Edit", &json!({ "message": "The commit message", "prefill": "fix:" }));
        assert_eq!((q.header.as_str(), q.question.as_str(), q.initial_answer.as_deref()), ("Edit", "The commit message", Some("fix:")));
        assert_eq!(answer_text(&json!(["x", "y"])).as_deref(), Some("x"));
    }

    #[test]
    fn tools_become_commands_file_changes_or_tools() {
        let ok = json!({ "content": [{ "type": "text", "text": "hi\n" }], "details": { "exitCode": 0 } });
        match tool_work("bash", &json!({ "command": "echo hi" }), &ok, true, false) {
            Work::Command { input, output, exit_code } => assert_eq!((input.as_str(), output.as_deref(), exit_code), ("echo hi", Some("hi\n"), Some(0))),
            _ => panic!("not a command"),
        }
        let patch = json!({ "content": [], "details": { "patch": "--- a\n+++ b\n" } });
        match tool_work("edit", &json!({ "path": "/w/src/a.rs", "oldText": "x", "newText": "y" }), &patch, true, false) {
            Work::FileChange { file_name, patch, texts, .. } => {
                assert_eq!(file_name, "a.rs");
                assert_eq!(patch.as_deref(), Some("--- a\n+++ b\n"));
                assert_eq!(texts, Some(("x".into(), "y".into())));
            }
            _ => panic!("not a file change"),
        }
        match tool_work("read", &json!({ "path": "a" }), &json!({ "content": [{ "type": "text", "text": "body" }] }), true, false) {
            Work::Tool { output, .. } => assert_eq!(output, Some(json!("body"))),
            _ => panic!("not a tool"),
        }
        assert_eq!(extension_name(Some("/x/ext/index.ts")), "ext");
        assert_eq!(extension_name(Some("/x/caches/domovoi-pi-extension.ts")), "domovoi-pi-extension");
    }

    #[test]
    fn the_extension_is_written_once_with_the_server_name() {
        let u = std::env::temp_dir().join(format!("domovoi-pi-ext-{}", crate::util::new_uuid()));
        let p = write_extension(&u).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(&format!("const SERVER = \"{}\";", crate::mcp::SERVER_NAME)) && !text.contains("__DOMOVOI_MCP_SERVER__"));
        assert_eq!(write_extension(&u).unwrap(), p);
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "nothing staged is left");
        let _ = std::fs::remove_dir_all(u);
    }
}
