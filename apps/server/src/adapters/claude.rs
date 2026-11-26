//! Claude adapter over the `claude` CLI's stream-json protocol. One CLI
//! process per thread, kept alive between turns (as the Agent SDK does).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Claude CLI's stream-json messages, tool inputs and answer format")]

use super::turn::{Plan, Turn, TurnEnd, WindowUse, Work, decision_allows};
use super::{Control, TurnInput};
use crate::claude_cli::{ClaudeCli, CliEvent, LaunchOptions};
use crate::orchestration::commands::Answers;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use domovoi_model::typed::{
    BackgroundTaskKind, FileChangeDetail, ModelSelection, OptionValue, PendingBackgroundTask, PlanStep, PlanStepStatus, ProviderRequestKind,
    ProviderSessionStatus, ProviderThreadStatus, QuestionOption, ThreadId, TurnItemStatus, UserInputQuestion,
};
use tokio::sync::{Mutex, mpsc, oneshot};

pub struct ClaudeSession {
    cli: Arc<ClaudeCli>,
    events: Mutex<mpsc::UnboundedReceiver<CliEvent>>,
    pub session_id: String,
    /// The folder the CLI runs in: a thread that moved needs a new process.
    pub cwd: String,
    pub permission_mode: Mutex<String>,
    /// The model's context window as the CLI last reported it (`modelUsage`).
    reported_window: std::sync::Mutex<Option<u64>>,
    /// Reads the CLI between turns (see `start_idle_drain`): its stop signal.
    idle: std::sync::Mutex<Option<(oneshot::Sender<()>, tokio::task::JoinHandle<()>)>>,
    /// The CLI exited, or a request it waits on could not be answered: the
    /// next turn starts a fresh (resumed) process instead.
    broken: AtomicBool,
    /// The CLI echoes user messages (`--replay-user-messages`): output before
    /// a turn's echo is the previous turn's.
    echoes: AtomicBool,
    plans_directory: PathBuf,
    plan_file: std::sync::Mutex<Option<PathBuf>>,
    tasks: std::sync::Mutex<Vec<PlanStep>>,
    /// What the thread's "Allow for Thread" answers allowed (earlier sessions'
    /// and this one's): requests these cover are allowed without asking,
    /// plan mode included, since Claude asks again for some anyway.
    granted: std::sync::Mutex<Vec<String>>,
}

/// The answer to a request Claude makes while no turn is open (nobody can see it).
const IDLE_DENY: &str = "The user wasn't in the conversation to answer this. Ask again in your reply; they will answer in their next message.";

/// The CLI's permission mode: plan mode (look and propose, change nothing)
/// when the thread plans first, else the runtime mode's.
pub fn mode_for(input: &TurnInput) -> &'static str {
    thread_mode(&input.interaction_mode, &input.runtime_mode)
}

/// [`mode_for`] from the thread's two modes.
pub fn thread_mode(interaction_mode: &str, runtime_mode: &str) -> &'static str {
    if interaction_mode == "plan" { "plan" } else { permission_mode(runtime_mode) }
}

pub fn permission_mode(runtime_mode: &str) -> &'static str {
    match runtime_mode {
        "approval-required" => "default",
        "auto-accept-edits" | "auto" => "acceptEdits",
        _ => "bypassPermissions",
    }
}

/// The choice the selection makes for the first of its options named one of `ids`.
pub(super) fn chosen<'a>(model_selection: &'a ModelSelection, ids: &[&str]) -> Option<&'a str> {
    match &model_selection.options.as_ref()?.iter().find(|option| ids.contains(&option.id.as_str()))?.value {
        OptionValue::Choice(choice) => Some(choice),
        OptionValue::Switch(_) => None,
    }
}

fn effort(model_selection: &ModelSelection) -> Option<String> {
    chosen(model_selection, &["effort", "reasoningEffort"]).map(str::to_owned)
}

/// The context window the selection asks for (its `contextWindow` option).
fn selected_window(model_selection: &ModelSelection) -> Option<u64> {
    let v = chosen(model_selection, &["contextWindow"])?.to_ascii_lowercase();
    let (num, mult) = match v.strip_suffix('m') {
        Some(n) => (n, 1_000_000.0),
        None => (v.strip_suffix('k')?, 1_000.0),
    };
    num.parse::<f64>().ok().map(|n| (n * mult) as u64)
}

/// A Claude message's `usage` as t3code's context use: everything sent
/// (fresh, cache writes and cache reads) plus what came back. `None` for a
/// placeholder with no input.
pub fn claude_token_usage(usage: &Value, max_tokens: Option<u64>) -> Option<WindowUse> {
    let n = |k: &str| usage[k].as_u64().unwrap_or(0);
    let input = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
    if input == 0 {
        return None;
    }
    let output = n("output_tokens");
    Some(WindowUse {
        used_tokens: input + output,
        max_tokens,
        input_tokens: input,
        cached_input_tokens: n("cache_read_input_tokens"),
        output_tokens: output,
        reasoning_output_tokens: 0,
    })
}

const PLAN_NOT_CAPTURED: &str = "The client could not capture the proposed plan. Submit it again with the full plan text, or share it in your reply.";
const MAX_PLAN_BYTES: u64 = 256 * 1024;

async fn plans_directory(cwd: &Path) -> PathBuf {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|path| !path.is_empty())
        .map(|path| cwd.join(path))
        .unwrap_or_else(|| crate::util::home_dir().join(".claude"));
    let configured = async {
        let settings = tokio::fs::read(config.join("settings.json")).await.ok()?;
        let settings = serde_json::from_slice::<Value>(&settings).ok()?;
        Some(cwd.join(settings["plansDirectory"].as_str().filter(|s| !s.is_empty())?))
    }.await;
    configured.unwrap_or_else(|| config.join("plans"))
}

/// Read only a confirmed plan write under Claude's plan directory.
async fn plan_snapshot(directory: &Path, path: &Path) -> Option<(PathBuf, String)> {
    if !path.is_absolute() || path.extension().and_then(|s| s.to_str()) != Some("md") {
        return None;
    }
    let directory = tokio::fs::canonicalize(directory).await.ok()?;
    let path = tokio::fs::canonicalize(path).await.ok()?;
    if !path.starts_with(&directory) || !tokio::fs::metadata(&path).await.ok()?.is_file() {
        return None;
    }
    // No O_NOFOLLOW on Windows: a link put there since is refused here.
    #[cfg(windows)]
    if crate::util::is_link(&path) {
        return None;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(&path).await.ok()?;
    if !file.metadata().await.ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLAN_BYTES + 1).read_to_end(&mut bytes).await.ok()?;
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return None;
    }
    // A plan written on Windows has CRLF line endings: shown alike everywhere.
    let markdown = String::from_utf8(bytes).ok()?.replace("\r\n", "\n");
    let markdown = markdown.trim();
    (!markdown.is_empty()).then(|| (path, markdown.to_owned()))
}

fn plan_step(id: &str, text: &str, status: PlanStepStatus) -> PlanStep {
    PlanStep { id: id.to_owned(), text: text.to_owned(), status, duration_anchor_at: None, duration_ms: None }
}

fn checklist_status(s: &str) -> Option<PlanStepStatus> {
    match s {
        "completed" => Some(PlanStepStatus::Completed),
        "in_progress" => Some(PlanStepStatus::Running),
        "pending" => Some(PlanStepStatus::Pending),
        _ => None,
    }
}

/// Apply a successful Claude task result to its session's checklist.
fn apply_task_tool(tasks: &mut Vec<PlanStep>, name: &str, input: &Value, result: &Value, output: &str) -> Option<()> {
    let text = |value: &Value| value.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let id = |value: &Value| text(value).or_else(|| value.as_u64().map(|n| n.to_string()));
    match name {
        "TaskList" => {
            let list = result["tasks"].as_array()?;
            let steps = list.iter().map(|task| {
                Some(plan_step(&id(&task["id"])?, &text(&task["subject"])?, checklist_status(task["status"].as_str()?)?))
            }).collect::<Option<Vec<_>>>()?;
            *tasks = steps;
        }
        "TaskCreate" => {
            let receipt = output.strip_prefix("Task #").and_then(|s| s.split_once(" created successfully: "));
            let id = id(&result["task"]["id"]).or_else(|| receipt.map(|(id, _)| id.to_owned()))?;
            let subject = text(&result["task"]["subject"]).or_else(|| text(&input["subject"]))
                .or_else(|| receipt.map(|(_, subject)| subject.trim().to_owned()).filter(|s| !s.is_empty()))?;
            if tasks.iter().any(|step| step.id == id) {
                return None;
            }
            tasks.push(plan_step(&id, &subject, PlanStepStatus::Pending));
        }
        "TaskUpdate" => {
            if result["success"] == false || (!result.is_object() && !output.starts_with("Updated task #")) {
                return None;
            }
            let id = id(&input["taskId"]).or_else(|| id(&result["taskId"]))?;
            let index = tasks.iter().position(|step| step.id == id)?;
            if input["status"] == "deleted" {
                tasks.remove(index);
            } else {
                let step = &mut tasks[index];
                if let Some(subject) = text(&input["subject"]) {
                    step.text = subject;
                }
                if let Some(status) = input["status"].as_str().and_then(checklist_status) {
                    step.status = status;
                }
            }
        }
        _ => return None,
    }
    Some(())
}

const PLAN_CAPTURED: &str = "The client captured your proposed plan. Stop here and wait for the user's feedback or implementation request in a later turn.";

/// `TodoWrite`'s todos as plan steps.
pub fn todo_steps(input: &Value) -> Vec<PlanStep> {
    input["todos"].as_array().into_iter().flatten().enumerate().filter_map(|(i, t)| {
        let text = t["content"].as_str().map(str::trim).filter(|c| !c.is_empty())?;
        let status = match t["status"].as_str() {
            Some("completed") => PlanStepStatus::Completed,
            Some("in_progress") => PlanStepStatus::Running,
            _ => PlanStepStatus::Pending,
        };
        Some(PlanStep { id: format!("step-{}", i + 1), text: text.to_owned(), status, duration_anchor_at: None, duration_ms: None })
    }).collect()
}

const DISMISSED: &str = "The user dismissed the question without answering. Carry on with your best judgment, or ask again in your reply.";

/// `AskUserQuestion` input as t3code's user-input questions: each question's
/// text is its id (the answers are keyed by it), headers default to
/// "Question N", options need a label (the description defaults to it).
pub fn user_input_questions(input: &Value) -> Vec<UserInputQuestion> {
    input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, q)| {
            let question = q["question"].as_str().map(str::trim).filter(|t| !t.is_empty())?;
            let header = q["header"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("Question {}", i + 1));
            let options = q["options"].as_array().into_iter().flatten().filter_map(|o| {
                let label = o["label"].as_str().map(str::trim).filter(|t| !t.is_empty())?;
                let description = o["description"].as_str().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(label);
                Some(QuestionOption { label: label.to_owned(), description: description.to_owned(), value: None, preview: o["preview"].as_str().map(str::to_owned) })
            }).collect();
            Some(UserInputQuestion {
                id: question.to_owned(),
                header,
                question: question.to_owned(),
                options,
                multi_select: Some(q["multiSelect"] == true),
                allow_custom_answer: None,
                initial_answer: None,
                required: None,
            })
        })
        .collect()
}

/// The user's answers (keyed by question, each in the shape the client gave
/// it) as the CLI takes them: one string each, several choices joined with ", ".
pub fn cli_answers(answers: &Answers) -> Value {
    let map = answers.iter().map(|(q, a)| {
        let text = match a {
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        (q.clone(), json!(text))
    });
    Value::Object(map.collect())
}

const ASK_IN_REPLY: &str = "This app can't show a question form. Ask the user in your reply instead: write each question with its options as text, then end your turn; they will answer in their next message.";

fn request_kind(tool: &str) -> ProviderRequestKind {
    match tool {
        "Bash" => ProviderRequestKind::Command,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ProviderRequestKind::FileChange,
        "Read" | "Glob" | "Grep" => ProviderRequestKind::FileRead,
        _ => ProviderRequestKind::Permission,
    }
}

impl ClaudeSession {
    pub async fn start(input: &TurnInput, session_id: &str, resume: bool, resume_at: Option<String>, log_dir: std::path::PathBuf) -> Result<Arc<Self>, String> {
        let mode = mode_for(input).to_owned();
        // What "Allow for Thread" allowed in this thread's earlier sessions.
        let granted = crate::thread_permissions::store()
            .map(|store| crate::thread_permissions::rules(store, &input.thread_id, crate::thread_permissions::CLAUDE))
            .unwrap_or_default();
        let opts = LaunchOptions {
            binary: input.claude_binary.clone(),
            cwd: input.cwd.clone(),
            model: input.model_selection.model.clone(),
            session_id: session_id.to_owned(),
            resume,
            resume_at,
            permission_mode: mode.clone(),
            effort: effort(&input.model_selection),
            extra_dirs: vec![],
            fork_from: input.fork.as_ref().filter(|_| !resume).map(|(src, _)| src.clone()),
            mcp_config: crate::mcp::claude_config(&input.thread_id),
            append_system_prompt: crate::mcp::enabled().then(crate::agent_instructions::system_prompt),
            enable_artifacts: input.claude_artifacts,
            allowed_tools: {
                let mut tools: Vec<String> = if crate::mcp::enabled() {
                    crate::mcp::read_only_tools().iter().map(|t| format!("mcp__{}__{t}", crate::mcp::SERVER_NAME)).collect()
                } else {
                    vec![]
                };
                tools.extend(granted.iter().cloned());
                tools
            },
        };
        let log_path = log_dir.join(format!("claude-{session_id}.ndjson"));
        let log = crate::logging::protocol_log(log_path);
        let (cli, events) = ClaudeCli::spawn(&opts, log).map_err(|e| format!("could not start claude: {e}"))?;
        let initialized = cli.initialize().await.map_err(|e| format!("claude initialize failed: {e}"))?;
        crate::skills::remember_descriptions(&initialized);
        // The account's limits now, before a turn reports any.
        crate::claude_limits::read_from(cli.clone(), input.model_selection.instance_id.as_str());
        Ok(Arc::new(ClaudeSession {
            cli, events: Mutex::new(events), session_id: session_id.to_owned(), cwd: input.cwd.clone(), permission_mode: Mutex::new(mode),
            plans_directory: plans_directory(Path::new(&input.cwd)).await, plan_file: Default::default(), tasks: Default::default(),
            reported_window: Default::default(), idle: Default::default(), broken: AtomicBool::new(false), echoes: AtomicBool::new(false),
            granted: std::sync::Mutex::new(granted),
        }))
    }

    /// The process can take another turn.
    pub fn usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst)
    }

    /// Between turns Claude can still speak (a background shell waking it,
    /// a turn stopped as an agent reported): read it, and decline anything
    /// it asks, so it never sits blocked on a question nobody sees and its
    /// words never land in the next turn. The next `run_turn` stops this.
    pub fn start_idle_drain(self: &Arc<Self>) {
        let (stop, mut stopped) = oneshot::channel();
        let me = self.clone();
        let task = tokio::spawn(async move {
            let mut events = me.events.lock().await;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    ev = events.recv() => match ev {
                        None | Some(CliEvent::Exited) => {
                            me.broken.store(true, Ordering::SeqCst);
                            break;
                        }
                        Some(CliEvent::PermissionRequest { request_id, request }) => me.decline_idle(&request_id, &request).await,
                        // A turn Claude started by itself after its run ended: what it
                        // says is in Claude's own transcript, not the thread's.
                        Some(CliEvent::Message(msg)) if msg["type"] == "system" && msg["subtype"] == "init" => {
                            tracing::warn!(session = %me.session_id, "Claude started a turn of its own after its run ended; the thread does not record it");
                        }
                        Some(CliEvent::Message(_)) => {}
                    },
                }
            }
        });
        *crate::util::locked(&self.idle) = Some((stop, task));
    }

    async fn stop_idle_drain(&self) {
        let running = crate::util::locked(&self.idle).take();
        if let Some((stop, task)) = running {
            let _ = stop.send(());
            let _ = task.await;
        }
    }

    async fn decline_idle(&self, request_id: &str, request: &Value) {
        tracing::info!(session = %self.session_id, tool = %request["tool_name"], "declined a request Claude made outside a turn");
        if self.cli.answer_permission(request_id, request, false, Some(IDLE_DENY)).await.is_err() {
            self.broken.store(true, Ordering::SeqCst);
        }
    }

    /// The thread's runtime or interaction mode changed (even mid-run).
    /// If Claude refuses the new mode, the next turn starts a fresh process in it.
    pub async fn set_thread_mode(&self, interaction_mode: &str, runtime_mode: &str) {
        if !self.set_mode(thread_mode(interaction_mode, runtime_mode)).await {
            self.broken.store(true, Ordering::SeqCst);
        }
    }

    /// Puts Claude in the mode this turn needs; false if it refused.
    pub async fn prepare(&self, input: &TurnInput) -> bool {
        self.set_mode(mode_for(input)).await
    }

    async fn set_mode(&self, mode: &'static str) -> bool {
        let mut current = self.permission_mode.lock().await;
        if *current == mode {
            return true;
        }
        match self.cli.set_permission_mode(mode).await {
            Ok(()) => {
                *current = mode.to_owned();
                true
            }
            Err(e) => {
                tracing::warn!(session = %self.session_id, mode, "Claude refused the permission mode: {e}");
                false
            }
        }
    }

    pub async fn kill(&self) {
        self.cli.kill().await;
    }

    /// Runs one turn. Returns the turn and whether the process is still usable.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> (Turn, bool) {
        let model = input.model_selection.model.clone();
        // The mode is already the turn's: `Host::claude_session` launched or
        // prepared the process in it, in the thread's effect lane. Setting it
        // again from `input` here (in the spawned turn) could undo a mode
        // change the lane applied since; only that change moves it now.
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &model, None);
        turn.thread_status(ProviderThreadStatus::Active, Some(&self.session_id));
        self.stop_idle_drain().await;
        let mut events = self.events.lock().await;
        // What came since the last turn is not this one's.
        while let Ok(ev) = events.try_recv() {
            match ev {
                CliEvent::PermissionRequest { request_id, request } => self.decline_idle(&request_id, &request).await,
                CliEvent::Exited => {
                    turn.complete(TurnEnd::Failed, Some("Claude Code exited."));
                    return (turn, false);
                }
                CliEvent::Message(_) => {}
            }
        }
        // A `$skill` mention: Claude expands `/skill` at the start of the last text block.
        let sent = match crate::skills::claude_dispatch(&input.text, &crate::skills::claude()) {
            Some(blocks) => self.cli.send_user_blocks(&blocks, &input.images).await,
            None if input.images.is_empty() => self.cli.send_user_message(&input.text).await,
            None => self.cli.send_user_message_with_images(&input.text, &input.images).await,
        };
        let message_uuid = match sent {
            Ok(uuid) => uuid,
            Err(e) => {
                turn.complete(TurnEnd::Failed, Some(&format!("Could not send the message to Claude: {e}")));
                return (turn, false);
            }
        };
        turn.turn_started(&crate::util::new_uuid());
        // Until Claude echoes this message, its output finishes an earlier
        // turn (one it started by itself): not this turn's.
        let mut echoed = !self.echoes.load(Ordering::SeqCst);
        let stale = |m: &Value, echoed: bool| !echoed && m["parent_tool_use_id"].is_null() && matches!(m["type"].as_str(), Some("assistant" | "stream_event" | "result"));
        // native item id (message id + content index) -> accumulated text / kind
        let mut blocks: HashMap<String, (String, String)> = HashMap::new();
        let mut current_message = String::new();
        let mut index_ids: HashMap<u64, String> = HashMap::new();
        // Each streamed message's blocks, in stream order: its `assistant`
        // message may arrive after the next one started streaming.
        let mut message_blocks: HashMap<String, Vec<(u64, String)>> = HashMap::new();
        // t3code request id -> (CLI request id, original request)
        let mut pending: HashMap<String, (String, Value)> = HashMap::new();
        let plan_key = format!("claude-plan:{}", turn.run.id);
        let mut plan_submitted = false;
        let tasks_key = format!("claude-checklist:{}", turn.run.id);
        let mut tool_inputs: HashMap<String, (String, Value)> = HashMap::new();
        // Streaming artifact tool calls (`Artifact`, `mcp__domovoi__artifact_*`): tool_use id -> (name, partial json)
        let mut streaming_artifact_tools: HashMap<String, (String, String)> = HashMap::new();
        let mut last_assistant_uuid: Option<String> = None;
        // Streamed blocks already completed by an `assistant` message.
        let mut finalized: std::collections::HashSet<String> = Default::default();
        let mut assistant_snapshots: std::collections::HashSet<String> = Default::default();
        // The user asked to stop: the turn ending in an error is that stop.
        let mut interrupt_requested = false;
        // Steers sent this run that Claude has queued as commands of their own
        // and not yet started (`command_lifecycle`). Claude ends the command
        // it was working on with a `result` and then starts the steer: that
        // result ends no run, the steered command's does.
        let mut queued_steers: std::collections::HashSet<String> = Default::default();
        // The limits that turned this turn away, by type, and when each resets.
        let mut rejected_limits: HashMap<String, Option<domovoi_model::typed::Timestamp>> = HashMap::new();
        let mut sent_steers: std::collections::HashSet<String> = Default::default();
        let window = |s: &Self| selected_window(&input.model_selection).or(*crate::util::locked(&s.reported_window));
        // The latest model call's usage, sent again once the window is known.
        let mut last_usage: Option<Value> = None;
        let mut subs = super::subagents::Subagents::default();
        // Claude's task id (for `stop_task`) by the tool use that started it.
        let mut task_ids: HashMap<String, String> = HashMap::new();
        // What Claude runs in the background (tool use id -> description and
        // kind): subagents, and commands it put in the background. Its reply
        // ends Claude's turn but not the run, which stays open until each
        // reports back (`task_notification`); Claude then carries on by itself,
        // in a turn of its own for each report it had not yet acted on.
        let mut background: std::collections::BTreeMap<String, (String, BackgroundTaskKind)> = Default::default();
        // Claude has replied and only background work is left, or a report
        // came in during its last turn: the turn Claude starts for it is this run's.
        let mut awaiting_background = false;
        // A background task reported since Claude's turn started (`system
        // init`): Claude may start another turn to act on it once this one ends.
        let mut reported_this_turn = false;
        // The last agent reported while Claude was between turns: Claude wakes
        // itself to act on it; if it doesn't by then, the run ends (t3code
        // CLAUDE_WAKE_TURN_START_DEADLINE).
        let mut wake_deadline: Option<tokio::time::Instant> = None;
        let pending_tasks = |background: &std::collections::BTreeMap<String, (String, BackgroundTaskKind)>, task_ids: &HashMap<String, String>, subs: &super::subagents::Subagents| -> Vec<PendingBackgroundTask> {
            background.iter().map(|(tool, (description, kind))| PendingBackgroundTask {
                command: None,
                item_id: None,
                source_thread_id: None,
                task_id: task_ids.get(tool).cloned().unwrap_or_else(|| tool.clone()),
                description: Some(description.trim().to_owned()).filter(|d| !d.is_empty()),
                parent_task_id: None,
                native_item_id: Some(tool.clone()),
                awaited: Some(true),
                kind: Some(kind.clone()),
                provider_instance_id: None,
                child_thread_id: subs.child_of(tool).map(ThreadId::from),
            }).collect()
        };
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(wake_deadline.unwrap_or_else(tokio::time::Instant::now)), if wake_deadline.is_some() => {
                    turn.background_tasks(vec![]);
                    subs.end(&turn, true);
                    turn.complete(TurnEnd::Completed, None);
                    return (turn, true);
                }
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, answers } => {
                        if let Some((cli_id, req)) = pending.remove(&request_id) {
                            if req["tool_name"] == "AskUserQuestion" {
                                let given = answers.as_ref().filter(|a| !a.is_empty());
                                match given.filter(|_| decision_allows(&decision)) {
                                    Some(a) => {
                                        let input = json!({ "questions": req["input"]["questions"], "answers": cli_answers(a) });
                                        let _ = self.cli.answer_permission_with_input(&cli_id, &req, input).await;
                                    }
                                    None => {
                                        let _ = self.cli.answer_permission(&cli_id, &req, false, Some(DISMISSED)).await;
                                    }
                                }
                                turn.resolve_request(&request_id, &decision, given);
                            } else {
                                let _ = if matches!(decision.as_str(), "acceptForSession" | "acceptAlways") {
                                    // Kept for this session's later requests and the thread's later sessions.
                                    let rules = crate::thread_permissions::claude_rules(&crate::claude_cli::session_permission_updates(&req));
                                    if let Some(store) = crate::thread_permissions::store() {
                                        crate::thread_permissions::remember(store, &input.thread_id, crate::thread_permissions::CLAUDE, &rules);
                                    }
                                    {
                                        let mut granted = crate::util::locked(&self.granted);
                                        for rule in rules {
                                            if !granted.contains(&rule) {
                                                granted.push(rule);
                                            }
                                        }
                                    }
                                    self.cli.answer_permission_for_session(&cli_id, &req).await
                                } else {
                                    self.cli.answer_permission(&cli_id, &req, decision_allows(&decision), None).await
                                };
                                turn.resolve_request(&request_id, &decision, None);
                            }
                        }
                    }
                    Control::Interrupt => {
                        interrupt_requested = true;
                        // Background agents stop with the run.
                        for tool in background.keys() {
                            if let Some(task) = task_ids.get(tool) {
                                let _ = self.cli.control(json!({ "subtype": "stop_task", "task_id": task })).await;
                            }
                        }
                        if awaiting_background {
                            // Claude is between turns: nothing to interrupt, the run just ends.
                            background.clear();
                            turn.background_tasks(vec![]);
                            subs.end(&turn, false);
                            turn.complete(TurnEnd::Interrupted, None);
                            return (turn, true);
                        }
                        let _ = self.cli.interrupt().await;
                    }
                    Control::Steer { text, images, reply } => {
                        let sent = self.cli.send_steer(&text, &images).await;
                        if let Ok(uuid) = &sent {
                            sent_steers.insert(uuid.clone());
                        }
                        let _ = reply.send(sent.map(|_| ()).map_err(|e| e.to_string()));
                    }
                    Control::StopSubagent { native_id, reply } => {
                        let res = match task_ids.get(&native_id) {
                            Some(task) => self.cli.control(json!({ "subtype": "stop_task", "task_id": task })).await.map(|_| ()).map_err(|e| e.to_string()),
                            None => Err("Claude has not reported this subagent's task yet.".into()),
                        };
                        let _ = reply.send(res);
                    }
                },
                ev = events.recv() => match ev {
                    None | Some(CliEvent::Exited) => {
                        turn.complete(TurnEnd::Failed, Some("Claude Code exited."));
                        return (turn, false);
                    }
                    Some(CliEvent::PermissionRequest { request_id, request }) if !echoed => {
                        self.decline_idle(&request_id, &request).await;
                    }
                    Some(CliEvent::PermissionRequest { request_id, request }) => {
                        let tool = request["tool_name"].as_str().unwrap_or("tool").to_owned();
                        // Claude's question tool: a question form the user answers
                        // with options (or asking in the reply, when it has none).
                        if tool == "AskUserQuestion" {
                            let questions = user_input_questions(&request["input"]);
                            if questions.is_empty() {
                                let _ = self.cli.answer_permission(&request_id, &request, false, Some(ASK_IN_REPLY)).await;
                                continue;
                            }
                            let item = request["tool_use_id"].as_str().unwrap_or(&request_id).to_owned();
                            let rid = turn.open_user_input(&request_id, &item, questions);
                            pending.insert(rid, (request_id, request));
                            continue;
                        }
                        let item = request["tool_use_id"].as_str().unwrap_or(&request_id).to_owned();
                        // Leaving plan mode: the plan becomes a proposal the user can
                        // implement; Claude stops here (t3code ClaudeAdapterV2).
                        if tool == "ExitPlanMode" {
                            let tool_input = &request["input"];
                            let mut markdown = tool_input["plan"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned);
                            if markdown.is_none()
                                && let Some(path) = tool_input["planFilePath"].as_str().map(Path::new)
                            {
                                let observed = crate::util::locked(&self.plan_file).clone();
                                if let Some(observed) = observed
                                    && tokio::fs::canonicalize(path).await.ok().as_ref() == Some(&observed)
                                {
                                    markdown = plan_snapshot(&self.plans_directory, &observed).await.map(|(_, markdown)| markdown);
                                }
                            }
                            let answer = if let Some(markdown) = markdown {
                                turn.plan(&plan_key, Plan::Proposed { markdown }, true);
                                plan_submitted = true;
                                PLAN_CAPTURED
                            } else {
                                turn.item(&format!("plan-handoff:{item}"), Work::Notice {
                                    message: "Claude tried to submit a plan, but its text could not be captured.".into(),
                                }, TurnItemStatus::Completed);
                                PLAN_NOT_CAPTURED
                            };
                            let _ = self.cli.answer_permission(&request_id, &request, false, Some(answer)).await;
                            continue;
                        }
                        // The user already allowed this for the thread: Claude asks
                        // again in some modes (plan mode ignores allow rules).
                        let granted = crate::thread_permissions::allows(&crate::util::locked(&self.granted), &request);
                        if granted {
                            tracing::info!(session = %self.session_id, %tool, "allowed by the thread's kept rules");
                            if self.cli.answer_permission(&request_id, &request, true, None).await.is_err() {
                                self.broken.store(true, Ordering::SeqCst);
                            }
                            continue;
                        }
                        let prompt = request["description"].as_str().map(str::to_owned)
                            .or_else(|| request["input"]["command"].as_str().map(str::to_owned))
                            .unwrap_or_else(|| format!("Use {tool}"));
                        let rid = turn.open_request(&request_id, &item, request_kind(&tool), &prompt);
                        pending.insert(rid, (request_id, request));
                    }
                    Some(CliEvent::Message(msg)) if msg["isReplay"] == true => {
                        self.echoes.store(true, Ordering::SeqCst);
                        if msg["uuid"].as_str() == Some(message_uuid.as_str()) {
                            echoed = true;
                        }
                    }
                    Some(CliEvent::Message(msg)) if stale(&msg, echoed) => {}
                    Some(CliEvent::Message(msg)) => match msg["type"].as_str().unwrap_or_default() {
                        "system" if msg["subtype"] == "init" => {
                            // Claude woke itself (a background task reported back).
                            awaiting_background = false;
                            wake_deadline = None;
                            reported_this_turn = false;
                            crate::providers::remember_claude_commands(&msg);
                            if crate::skills::remember_claude(&msg, &input.cwd) {
                                crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
                            }
                        }
                        "system" if msg["subtype"] == "task_started" => {
                            if let (Some(task), Some(tool)) = (msg["task_id"].as_str(), msg["tool_use_id"].as_str()) {
                                task_ids.insert(tool.to_owned(), task.to_owned());
                                // Put in the background by Claude (a command as well as an
                                // agent): the run waits for it like for a background agent.
                                if msg["is_backgrounded"] == true && !background.contains_key(tool) {
                                    let description = msg["description"].as_str().unwrap_or_default().to_owned();
                                    background.insert(tool.to_owned(), (description, background_kind(msg["task_type"].as_str())));
                                }
                                if background.contains_key(tool) {
                                    turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
                                }
                            }
                        }
                        // Every task Claude still runs in the background: one that
                        // ended without its report is not waited for.
                        "system" if msg["subtype"] == "background_tasks_changed" => {
                            let running: std::collections::HashSet<&str> = msg["tasks"].as_array().into_iter().flatten().filter_map(|t| t["task_id"].as_str()).collect();
                            let before = background.len();
                            background.retain(|tool, _| task_ids.get(tool).is_none_or(|task| running.contains(task.as_str())));
                            if background.len() != before {
                                turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
                                reported_this_turn = true;
                                if awaiting_background && background.is_empty() && wake_deadline.is_none() {
                                    wake_deadline = Some(tokio::time::Instant::now() + WAKE_DEADLINE);
                                }
                            }
                        }
                        "system" if msg["subtype"] == "task_progress" => {
                            let tool = msg["tool_use_id"].as_str().map(str::to_owned)
                                .or_else(|| task_ids.iter().find(|(_, t)| msg["task_id"] == json!(t)).map(|(k, _)| k.clone()));
                            if let (Some(tool), Some(p)) = (tool, msg["description"].as_str().or(msg["summary"].as_str())) {
                                subs.progress(&turn, &tool, p);
                            }
                        }
                        "system" if msg["subtype"] == "task_notification" => {
                            let tool = msg["tool_use_id"].as_str().map(str::to_owned)
                                .or_else(|| task_ids.iter().find(|(_, t)| msg["task_id"] == json!(t)).map(|(k, _)| k.clone()));
                            if let Some(tool) = tool {
                                let status = task_status(msg["status"].as_str());
                                subs.finish(&turn, &tool, status, msg["summary"].as_str());
                                if background.remove(&tool).is_some() {
                                    reported_this_turn = true;
                                    turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
                                    if awaiting_background && background.is_empty() {
                                        wake_deadline = Some(tokio::time::Instant::now() + WAKE_DEADLINE);
                                    }
                                }
                            }
                        }
                        "system" if msg["subtype"] == "compact_boundary" => {
                            let id = format!("compact:{}", msg["uuid"].as_str().unwrap_or(&current_message));
                            let before_token_count = msg["compact_metadata"]["pre_tokens"].as_u64();
                            turn.item(&id, Work::Compaction { before_token_count }, TurnItemStatus::Completed);
                        }
                        "system" if msg["subtype"] == "api_retry" => {
                            let id = format!("retry:{}", msg["uuid"].as_str().unwrap_or(&current_message));
                            let attempt = msg["attempt"].as_u64().unwrap_or(1);
                            let message = match msg["max_retries"].as_u64() {
                                Some(max) => format!("Claude's API returned an error; retrying (attempt {attempt} of {max})."),
                                None => format!("Claude's API returned an error; retrying (attempt {attempt})."),
                            };
                            turn.item(&id, Work::Notice { message }, TurnItemStatus::Completed);
                        }
                        // A subagent's own stream: its messages arrive whole, below.
                        "stream_event" if !msg["parent_tool_use_id"].is_null() => {}
                        "stream_event" => {
                            let e = &msg["event"];
                            match e["type"].as_str().unwrap_or_default() {
                                "message_start" => {
                                    current_message = e["message"]["id"].as_str().unwrap_or_default().to_owned();
                                    index_ids.clear();
                                }
                                "content_block_start" => {
                                    let idx = e["index"].as_u64().unwrap_or(0);
                                    let block = &e["content_block"];
                                    let kind = block["type"].as_str().unwrap_or_default().to_owned();
                                    let id = if kind == "tool_use" { block["id"].as_str().unwrap_or_default().to_owned() } else { format!("{current_message}:{idx}") };
                                    index_ids.insert(idx, id.clone());
                                    let entry = message_blocks.entry(current_message.clone()).or_default();
                                    entry.push((idx, id.clone()));
                                    entry.sort();
                                    let text = if kind == "thinking" { block["thinking"].as_str().unwrap_or_default().to_owned() } else { String::new() };
                                    if kind == "thinking" && !text.trim().is_empty() {
                                        turn.item_streaming(&id, Work::Reasoning { text: text.clone(), streaming: true }, text.len());
                                    }
                                    if kind == "tool_use"
                                        && let Some(tool_name) = block["name"].as_str()
                                        && is_artifact_tool(tool_name)
                                    {
                                        streaming_artifact_tools.insert(id.clone(), (tool_name.to_owned(), String::new()));
                                        turn.item_streaming(
                                            &id,
                                            Work::Tool { name: tool_name.to_owned(), input: json!({ "_streaming": true }), output: None, viewed_image_path: None },
                                            0,
                                        );
                                    }
                                    blocks.insert(id, (kind, text));
                                }
                                "content_block_delta" => {
                                    let idx = e["index"].as_u64().unwrap_or(0);
                                    let Some(id) = index_ids.get(&idx).cloned() else { continue };
                                    let d = &e["delta"];
                                    if let Some((kind, text)) = blocks.get_mut(&id) {
                                        match d["type"].as_str() {
                                            Some("text_delta") => {
                                                turn.assistant_delta(&id, text, d["text"].as_str().unwrap_or_default());
                                            }
                                            Some("thinking_delta") if kind == "thinking" => {
                                                text.push_str(d["thinking"].as_str().unwrap_or_default());
                                                if !text.trim().is_empty() {
                                                    turn.item_streaming(&id, Work::Reasoning { text: text.clone(), streaming: true }, text.len());
                                                }
                                            }
                                            Some("input_json_delta") if kind == "tool_use" => {
                                                if let Some((tool_name, partial)) = streaming_artifact_tools.get_mut(&id) {
                                                    partial.push_str(d["partial_json"].as_str().unwrap_or_default());
                                                    let preview_input = partial_artifact_input(partial);
                                                    turn.item_streaming(
                                                        &id,
                                                        Work::Tool { name: tool_name.clone(), input: preview_input, output: None, viewed_image_path: None },
                                                        partial.len(),
                                                    );
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "content_block_stop" => {
                                    let idx = e["index"].as_u64().unwrap_or(0);
                                    if let Some(id) = index_ids.get(&idx)
                                        && let Some((kind, text)) = blocks.get(id)
                                        && kind == "thinking" && !text.trim().is_empty()
                                    {
                                        // Keep the buffer for the later assistant snapshot: an
                                        // empty snapshot must not erase what the stream delivered.
                                        turn.item(id, Work::Reasoning { text: text.clone(), streaming: false }, TurnItemStatus::Completed);
                                    }
                                }
                                _ => {}
                            }
                        }
                        "assistant" => {
                            if let Some(uuid) = msg["uuid"].as_str()
                                && !assistant_snapshots.insert(uuid.to_owned())
                            {
                                continue;
                            }
                            if msg["parent_tool_use_id"].is_null() {
                                if let Some(u) = msg["uuid"].as_str() {
                                    last_assistant_uuid = Some(u.to_owned());
                                }
                                if let Some(u) = claude_token_usage(&msg["message"]["usage"], window(self)) {
                                    last_usage = Some(msg["message"]["usage"].clone());
                                    turn.token_usage(u);
                                }
                            }
                            let mid = msg["message"]["id"].as_str().unwrap_or(&current_message).to_owned();
                            // A subagent's message goes to its own thread.
                            if let Some(owner) = msg["parent_tool_use_id"].as_str().filter(|p| subs.knows(p)).map(str::to_owned) {
                                for (idx, block) in msg["message"]["content"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                                    match block["type"].as_str().unwrap_or_default() {
                                        "text" => subs.text(&turn, &owner, &format!("{mid}:{idx}"), block["text"].as_str().unwrap_or_default()),
                                        "tool_use" => {
                                            let id = block["id"].as_str().unwrap_or_default().to_owned();
                                            let name = block["name"].as_str().unwrap_or("tool").to_owned();
                                            tool_inputs.insert(id.clone(), (name.clone(), block["input"].clone()));
                                            subs.record_work(&turn, &owner, &id, tool_work(&name, &block["input"], None, &Value::Null), TurnItemStatus::Running);
                                        }
                                        _ => {}
                                    }
                                }
                                continue;
                            }
                            // With partial messages the CLI sends one `assistant` message per content
                            // block, so a block's array index is not its stream index: complete the
                            // earliest streamed block of the same kind instead.
                            let streamed = |kind: &str, finalized: &std::collections::HashSet<String>| -> Option<String> {
                                message_blocks.get(&mid)?.iter().map(|(_, id)| id).find(|id| !finalized.contains(*id) && blocks.get(*id).is_some_and(|(k, _)| k == kind)).cloned()
                            };
                            for (idx, block) in msg["message"]["content"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                                match block["type"].as_str().unwrap_or_default() {
                                    "text" => {
                                        let id = streamed("text", &finalized).unwrap_or_else(|| format!("{mid}:{idx}"));
                                        finalized.insert(id.clone());
                                        turn.assistant(&id, block["text"].as_str().unwrap_or_default(), false, None);
                                    }
                                    "thinking" => {
                                        let id = streamed("thinking", &finalized).unwrap_or_else(|| format!("{mid}:{idx}"));
                                        finalized.insert(id.clone());
                                        let snapshot = block["thinking"].as_str().unwrap_or_default();
                                        let text = if snapshot.trim().is_empty() {
                                            blocks.get(&id).map(|(_, text)| text.as_str()).unwrap_or_default()
                                        } else {
                                            snapshot
                                        }.to_owned();
                                        if !text.trim().is_empty() {
                                            turn.item(&id, Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
                                        }
                                    }
                                    "tool_use" => {
                                        let id = block["id"].as_str().unwrap_or_default().to_owned();
                                        let name = block["name"].as_str().unwrap_or("tool").to_owned();
                                        tool_inputs.insert(id.clone(), (name.clone(), block["input"].clone()));
                                        if matches!(name.as_str(), "Task" | "Agent") && msg["parent_tool_use_id"].is_null() {
                                            let i = &block["input"];
                                            subs.start(&mut turn, &id, i["prompt"].as_str().unwrap_or_default(), i["description"].as_str(), i["model"].as_str());
                                            continue;
                                        }
                                        if name == "ExitPlanMode" && msg["parent_tool_use_id"].is_null() {
                                            continue;
                                        }
                                        record_tool(&mut turn, &id, &name, &block["input"], None, &Value::Null, TurnItemStatus::Running);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "user" => {
                            for block in msg["message"]["content"].as_array().cloned().unwrap_or_default() {
                                if block["type"] != "tool_result" { continue }
                                let id = block["tool_use_id"].as_str().unwrap_or_default().to_owned();
                                let Some((name, tool_input)) = tool_inputs.get(&id).cloned() else { continue };
                                let output = match &block["content"] {
                                    Value::String(s) => s.clone(),
                                    Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
                                    _ => String::new(),
                                };
                                let structured = &msg["tool_use_result"];
                                let no_matches = name == "Bash" && structured["returnCodeInterpretation"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("no matches found"));
                                let failed = block["is_error"] == true && !no_matches;
                                let status = if name == "Bash" && structured["interrupted"] == true {
                                    TurnItemStatus::Interrupted
                                } else if failed {
                                    TurnItemStatus::Failed
                                } else {
                                    TurnItemStatus::Completed
                                };
                                let image_marker = serde_json::json!({ "type": "image" });
                                let structured = if name == "Read" && !failed && structured["type"] != "image"
                                    && block["content"].as_array().is_some_and(|parts| parts.iter().any(|part| part["type"] == "image"))
                                {
                                    &image_marker
                                } else {
                                    structured
                                };
                                // Started in the background: the result is only a receipt
                                // (internal, not for the user); the agent reports later.
                                if subs.knows(&id) && !failed && is_background_launch(&output) {
                                    background.insert(id.clone(), (tool_input["description"].as_str().unwrap_or_default().to_owned(), BackgroundTaskKind::Subagent));
                                    subs.progress(&turn, &id, "Running in the background");
                                    turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
                                    continue;
                                }
                                if subs.knows(&id) {
                                    subs.finish(&turn, &id, if failed { "failed" } else { "completed" }, Some(&output));
                                    continue;
                                }
                                if let Some(owner) = msg["parent_tool_use_id"].as_str().filter(|p| subs.knows(p)) {
                                    subs.record_work(&turn, owner, &id, tool_work(&name, &tool_input, Some((&output, failed)), structured), status);
                                    continue;
                                }
                                if msg["parent_tool_use_id"].is_null() {
                                    if name == "ExitPlanMode" {
                                        continue;
                                    }
                                    if !failed {
                                        if name == "TodoWrite" && tool_input["todos"].is_array() {
                                            turn.plan(&tasks_key, Plan::Todo { steps: todo_steps(&tool_input), explanation: None }, true);
                                        }
                                        if matches!(name.as_str(), "TaskCreate" | "TaskUpdate" | "TaskList") {
                                            let mut tasks = crate::util::locked(&self.tasks);
                                            if apply_task_tool(&mut tasks, &name, &tool_input, &msg["tool_use_result"], &output).is_some() {
                                                turn.plan(&tasks_key, Plan::Todo { steps: tasks.clone(), explanation: None }, true);
                                            }
                                        }
                                        if !plan_submitted
                                            && matches!(name.as_str(), "Write" | "Edit" | "MultiEdit")
                                            && *self.permission_mode.lock().await == "plan"
                                            && let Some(path) = tool_input["file_path"].as_str().map(Path::new)
                                            && let Some((path, markdown)) = plan_snapshot(&self.plans_directory, path).await
                                        {
                                            *crate::util::locked(&self.plan_file) = Some(path);
                                            turn.plan(&plan_key, Plan::Proposed { markdown }, false);
                                        }
                                        if name == "Artifact" {
                                            capture_claude_artifact(&input.thread_id, &id, &tool_input, &output);
                                        }
                                    }
                                }
                                streaming_artifact_tools.remove(&id);
                                record_tool(&mut turn, &id, &name, &tool_input, Some((&output, failed)), structured, status);
                            }
                        }
                        // The bookkeeping result Claude sends for a task notice ends no turn.
                        "result" if awaiting_background && msg["subtype"] == "success" && msg["num_turns"] == 0 => {}
                        "rate_limit_event" => {
                            let info = &msg["rate_limit_info"];
                            crate::usage_limits::merge(input.model_selection.instance_id.as_str(), crate::usage_limits::claude_windows(info));
                            let kind = info["rateLimitType"].as_str().unwrap_or("unknown").to_owned();
                            if crate::usage_limits::claude_blocked(info) {
                                rejected_limits.insert(kind, crate::usage_limits::claude_reset_at(info));
                            } else {
                                rejected_limits.remove(&kind);
                            }
                        }
                        "command_lifecycle" => {
                            if let Some(command) = msg["command_uuid"].as_str().filter(|c| sent_steers.contains(*c)) {
                                if msg["state"] == "queued" {
                                    queued_steers.insert(command.to_owned());
                                } else {
                                    queued_steers.remove(command);
                                }
                            }
                        }
                        // The command a steer cut short ended; the steer runs next, in this run.
                        "result" if !queued_steers.is_empty() && !interrupt_requested => {}
                        "result" => {
                            let ok = msg["subtype"] == "success" && msg["is_error"] != true;
                            if !ok && !rejected_limits.is_empty() {
                                // The latest reset: resuming then finds every limit open again.
                                let reset = rejected_limits.values().flatten().max_by(|a, b| a.as_str().cmp(b.as_str())).cloned();
                                turn.usage_limited(reset);
                            }
                            // A stop: asked for here, or reported as one (`terminal_reason`
                            // `aborted_streaming`, or a result naming the interrupt).
                            let interrupted = msg["subtype"] == "error_during_execution"
                                && (interrupt_requested
                                    || msg["terminal_reason"].as_str().is_some_and(|r| r.starts_with("aborted"))
                                    || msg["result"].as_str().is_some_and(|r| r.contains("interrupt")));
                            let status = if ok { "completed" } else if interrupted { "interrupted" } else { "failed" };
                            let err = msg["result"].as_str().map(str::to_owned);
                            // `modelUsage` names the window; usage sent without one gets it now.
                            let reported = msg["modelUsage"].as_object().and_then(|m| m.values().find_map(|v| v["contextWindow"].as_u64()));
                            if let Some(w) = reported {
                                let had = window(self).is_some();
                                *crate::util::locked(&self.reported_window) = Some(w);
                                if let (false, Some(u)) = (had, last_usage.as_ref().and_then(|u| claude_token_usage(u, window(self)))) {
                                    turn.token_usage(u);
                                }
                            }
                            if let Some(cursor) = &last_assistant_uuid {
                                turn.set_turn_cursor(cursor);
                            }
                            // Claude replied but background work goes on: the run
                            // waits for it (and for what Claude does once it reports).
                            if status == "completed" && !background.is_empty() {
                                awaiting_background = true;
                                // Only commands left: one may never end (a dev server
                                // Claude started), so the run waits a while, not forever.
                                if background.values().all(|(_, kind)| *kind == BackgroundTaskKind::Command) {
                                    wake_deadline = Some(tokio::time::Instant::now() + COMMAND_WAIT);
                                }
                                continue;
                            }
                            // Nothing runs, but a task reported while Claude worked:
                            // Claude starts a turn of its own for that report, right
                            // after this one. It is this run's, if it comes soon.
                            if status == "completed" && reported_this_turn {
                                awaiting_background = true;
                                wake_deadline = Some(tokio::time::Instant::now() + REPORT_WAKE_GRACE);
                                continue;
                            }
                            if !background.is_empty() {
                                for tool in background.keys() {
                                    if let Some(task) = task_ids.get(tool) {
                                        let _ = self.cli.control(json!({ "subtype": "stop_task", "task_id": task })).await;
                                    }
                                }
                                background.clear();
                                turn.background_tasks(vec![]);
                            }
                            subs.end(&turn, status == "completed");
                            turn.complete(TurnEnd::named(status), err.as_deref());
                            // What the turn used of the account's limits, all of them.
                            crate::claude_limits::read_from(self.cli.clone(), input.model_selection.instance_id.as_str());
                            return (turn, true);
                        }
                        _ => {}
                    },
                },
            }
        }
    }
}

/// Whether `name` is an artifact-publishing tool whose streaming JSON should
/// update the transcript live while Claude generates it.
fn is_artifact_tool(name: &str) -> bool {
    // A session from before the tools were renamed still calls them `t3_…` of a server `t3`.
    matches!(name, "Artifact" | "mcp__domovoi__artifact_create" | "mcp__domovoi__artifact_update" | "mcp__t3__t3_artifact_create" | "mcp__t3__t3_artifact_update")
}

/// Extracts best-effort title/kind/id/content hints from a partial JSON tool
/// argument buffer while an artifact is streaming.
fn partial_artifact_input(partial: &str) -> Value {
    if let Ok(full) = serde_json::from_str::<Value>(partial) {
        return full;
    }
    let mut map = serde_json::Map::new();
    map.insert("_streaming".into(), Value::Bool(true));
    for key in ["id", "title", "kind", "language", "path", "action"] {
        if let Some(val) = extract_partial_json_string(partial, key) {
            map.insert(key.into(), Value::String(val));
        }
    }
    Value::Object(map)
}

fn extract_partial_json_string(src: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = src.find(&needle)?;
    let rest = src[start + needle.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(esc) = chars.next() {
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    other => out.push(other),
                }
            }
        } else if c == '"' {
            return (!out.is_empty()).then_some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Mirrors a completed Claude Code native `Artifact` publish/update call into
/// Domovoi's durable artifact store so it gets version history and instant
/// local preview alongside its hosted `claude.ai` link.
fn capture_claude_artifact(thread_id: &str, turn_item_id: &str, tool_input: &Value, output: &str) {
    let action = tool_input["action"].as_str().unwrap_or("publish");
    if !matches!(action, "publish" | "create" | "update") {
        return;
    }
    let Some(state) = crate::state::STATE.get() else { return };
    let Ok(Some(thread)) = state.store.app_thread(thread_id) else { return };
    let hosted_url = extract_claude_artifact_url(output)
        .or_else(|| tool_input["url"].as_str().map(str::to_owned));
    let content = tool_input["content"].as_str().unwrap_or_default();
    if content.is_empty() && hosted_url.is_none() {
        return;
    }
    let title = tool_input["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Claude Code Artifact");
    let kind = if !content.is_empty() {
        tool_input["kind"].as_str().or(tool_input["type"].as_str()).unwrap_or("html")
    } else {
        "hosted"
    };
    let artifact_id = tool_input["id"]
        .as_str()
        .or_else(|| hosted_url.as_deref().and_then(|u| u.rsplit('/').next()).filter(|s| !s.is_empty()));
    let _ = crate::artifacts::upsert(
        &state.store,
        crate::artifacts::UpsertInput {
            id: artifact_id,
            thread_id,
            project_id: thread.project_id.as_str(),
            title,
            kind,
            language: tool_input["language"].as_str(),
            source_path: tool_input["path"].as_str(),
            hosted_url: hosted_url.as_deref(),
            run_id: None,
            turn_item_id: Some(turn_item_id),
            summary: tool_input["description"].as_str(),
            content,
        },
    );
}

fn extract_claude_artifact_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | '.'));
        if cleaned.starts_with("https://claude.ai/") || cleaned.starts_with("https://www.claude.ai/") {
            return Some(cleaned.to_owned());
        }
    }
    None
}

/// How long Claude has to wake itself after its last background agent reports.
const WAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
/// How long a run stays open, once Claude's turn ended, for commands Claude
/// left running in the background (a build, a test run) to report. Agents
/// always report and are waited for; a command may run for good (a server),
/// and the thread is not left working for it.
const COMMAND_WAIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// How long a run waits, after Claude's turn ended with nothing left in the
/// background, for the turn Claude starts by itself for a report that came
/// in during it (it starts within milliseconds when it does; when Claude
/// already acted on the report, none comes).
const REPORT_WAKE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// A subagent's status from Claude's `task_notification` status.
pub fn task_status(status: Option<&str>) -> &'static str {
    match status {
        Some("stopped" | "killed") => "cancelled",
        Some("failed") => "failed",
        _ => "completed",
    }
}

/// What a task Claude put in the background is, by its `task_type`.
fn background_kind(task_type: Option<&str>) -> BackgroundTaskKind {
    match task_type {
        Some("local_bash") => BackgroundTaskKind::Command,
        Some("local_agent" | "remote_agent") => BackgroundTaskKind::Subagent,
        _ => BackgroundTaskKind::BackgroundTask,
    }
}

/// The tool result Claude gives for a subagent it started in the background
/// (`run_in_background`): a receipt, not the agent's answer.
pub fn is_background_launch(output: &str) -> bool {
    output.trim_start().starts_with("Async agent launched")
}

/// The before and after an edit tool call shows as its diff: `Edit`'s
/// strings, a new file's content (`Write`), or `MultiEdit`'s edits one
/// after another.
fn edit_texts(tool: &str, input: &Value) -> Option<(String, String)> {
    match tool {
        "Write" => Some((String::new(), input["content"].as_str()?.to_owned())),
        "MultiEdit" => {
            let edits = input["edits"].as_array()?;
            let pick = |k: &str| edits.iter().filter_map(|e| e[k].as_str()).collect::<Vec<_>>().join("\n");
            (!edits.is_empty()).then(|| (pick("old_string"), pick("new_string")))
        }
        _ => Some((input["old_string"].as_str()?.to_owned(), input["new_string"].as_str()?.to_owned())),
    }
}

/// Maps a Claude tool call onto a t3code turn item.
fn record_tool(turn: &mut Turn, id: &str, name: &str, input: &Value, result: Option<(&str, bool)>, structured: &Value, status: TurnItemStatus) {
    turn.item(id, tool_work(name, input, result, structured), status);
}

/// What a Claude tool call shows: `result` is its text and whether it
/// failed, `structured` Claude's `tool_use_result` (what the text leaves out:
/// an image's size, the tools a search loaded). An image's bytes are never
/// kept: the file it came from is named instead.
pub fn tool_work(name: &str, input: &Value, result: Option<(&str, bool)>, structured: &Value) -> Work {
    let failed = result.is_some_and(|(_, failed)| failed);
    match name {
        "Bash" => Work::Command {
            input: input["command"].as_str().unwrap_or_default().to_owned(),
            output: result.map(|(out, _)| {
                structured["returnCodeInterpretation"].as_str().filter(|_| out.trim().is_empty()).unwrap_or(out).to_owned()
            }),
            exit_code: result.map(|(out, failed)| {
                out.lines().next().and_then(|line| line.strip_prefix("Exit code ")).and_then(|code| code.trim().parse().ok()).unwrap_or(i64::from(failed))
            }),
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or("file");
            let file_name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.to_owned());
            let operation = if name == "Write" { "add" } else { "update" };
            let changes = vec![FileChangeDetail { operation: operation.into(), path: path.to_owned(), old_path: None, file_type: None, mime_type: None }];
            let error = result.filter(|_| failed).map(|(out, _)| out.trim().to_owned()).filter(|e| !e.is_empty());
            Work::FileChange { file_name, changes, texts: edit_texts(name, input), patch: None, error }
        }
        "Read" if structured["type"] == "image" => {
            let dims = &structured["file"]["dimensions"];
            let size = dims["originalWidth"].as_u64().zip(dims["originalHeight"].as_u64());
            let output = size.map_or_else(|| "Image".to_owned(), |(w, h)| format!("Image · {w}×{h}"));
            let path = input["file_path"].as_str().map(str::to_owned);
            Work::Tool { name: name.to_owned(), input: input.clone(), output: Some(Value::String(output)), viewed_image_path: path }
        }
        "WebSearch" if !failed => {
            let patterns = input["query"].as_str().or_else(|| structured["query"].as_str()).into_iter().map(str::to_owned).collect();
            let results = structured["results"].as_array().map(|groups| {
                groups.iter().flat_map(|entry| {
                    entry["content"].as_array().map_or(std::slice::from_ref(entry), Vec::as_slice)
                }).filter_map(|entry| {
                    let url = entry["url"].as_str()?.trim();
                    (!url.is_empty()).then(|| domovoi_model::typed::WebSearchResult {
                        title: entry["title"].as_str().map(str::to_owned),
                        url: Some(url.to_owned()),
                        snippet: entry["snippet"].as_str().map(str::to_owned),
                    })
                }).collect()
            });
            Work::WebSearch { patterns, results }
        }
        "ToolSearch" if structured["matches"].is_array() && result.is_some_and(|(out, _)| out.trim().is_empty()) => {
            let matches: Vec<&str> = structured["matches"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let output = if matches.is_empty() { "No matching tools".to_owned() } else { format!("Loaded {}", matches.join(", ")) };
            Work::Tool { name: name.to_owned(), input: input.clone(), output: Some(Value::String(output)), viewed_image_path: None }
        }
        _ => Work::Tool { name: name.to_owned(), input: input.clone(), output: result.map(|(out, _)| Value::String(out.to_owned())), viewed_image_path: None },
    }
}

#[cfg(test)]
mod tests {
    use super::{claude_token_usage, cli_answers, edit_texts, selected_window, tool_work, user_input_questions};
    use crate::adapters::turn::Work;

    #[test]
    fn tool_results_keep_what_their_text_leaves_out() {
        let image = json!({ "type": "image", "file": { "base64": "iVBOR", "type": "image/png", "dimensions": { "originalWidth": 256, "originalHeight": 128 } } });
        let Work::Tool { output, viewed_image_path, .. } = tool_work("Read", &json!({ "file_path": "/tmp/a.png" }), Some(("", false)), &image) else { panic!("a tool") };
        assert_eq!((output, viewed_image_path.as_deref()), (Some(json!("Image · 256×128")), Some("/tmp/a.png")), "the file, never the base64");
        let found = json!({ "matches": ["Monitor", "WebSearch"], "query": "select:Monitor,WebSearch" });
        let Work::Tool { output, .. } = tool_work("ToolSearch", &json!({ "query": "select:Monitor,WebSearch" }), Some(("", false)), &found) else { panic!("a tool") };
        assert_eq!(output, Some(json!("Loaded Monitor, WebSearch")));
        let web = json!({ "results": [{ "content": [{ "title": "Rust", "url": "https://www.rust-lang.org/" }] }] });
        let Work::WebSearch { patterns, results } = tool_work("WebSearch", &json!({ "query": "Rust" }), Some(("", false)), &web) else { panic!("a web search") };
        assert_eq!(patterns, ["Rust"]);
        let results = results.expect("results from Claude");
        assert_eq!(results[0].title.as_deref(), Some("Rust"));
        assert_eq!(results[0].url.as_deref(), Some("https://www.rust-lang.org/"));

        let Work::Command { exit_code, .. } = tool_work("Bash", &json!({ "command": "cargo test" }), Some(("Exit code 101\nTests failed", true)), &serde_json::Value::Null) else { panic!("a command") };
        assert_eq!(exit_code, Some(101));
        let edit = json!({ "file_path": "/w/a.rs", "old_string": "x", "new_string": "y" });
        let Work::FileChange { error, .. } = tool_work("Edit", &edit, Some(("String to replace not found in file.", true)), &serde_json::Value::Null) else { panic!("an edit") };
        assert_eq!(error.as_deref(), Some("String to replace not found in file."));
        let Work::FileChange { error, .. } = tool_work("Edit", &edit, Some(("The file has been updated.", false)), &serde_json::Value::Null) else { panic!("an edit") };
        assert_eq!(error, None);
    }

    #[test]
    fn usage_counts_everything_sent_against_the_selected_window() {
        let usage = json!({ "input_tokens": 10, "cache_creation_input_tokens": 200, "cache_read_input_tokens": 3000, "output_tokens": 40 });
        let u = claude_token_usage(&usage, Some(200_000)).unwrap();
        assert_eq!(u.used_tokens, 3250);
        assert_eq!(u.input_tokens, 3210);
        assert_eq!(u.cached_input_tokens, 3000);
        assert_eq!(u.max_tokens, Some(200_000));
        assert!(claude_token_usage(&json!({ "input_tokens": 0, "output_tokens": 5 }), None).is_none(), "a placeholder");
        let sel = |options: serde_json::Value| -> domovoi_model::typed::ModelSelection { serde_json::from_value(json!({ "instanceId": "claudeAgent", "model": "m", "options": options })).unwrap() };
        assert_eq!(selected_window(&sel(json!([{ "id": "contextWindow", "value": "1m" }]))), Some(1_000_000));
        assert_eq!(selected_window(&sel(json!([{ "id": "fast", "value": true }, { "id": "contextWindow", "value": "200k" }]))), Some(200_000));
        assert_eq!(selected_window(&sel(json!([{ "id": "contextWindow", "value": true }]))), None, "a switch is not a size");
        assert_eq!(selected_window(&sel(json!([]))), None);
        assert_eq!(super::effort(&sel(json!([{ "id": "reasoningEffort", "value": "high" }]))).as_deref(), Some("high"));
    }
    use serde_json::json;

    #[test]
    fn questions_become_user_input_questions_keyed_by_their_text() {
        let input = json!({ "questions": [
            { "question": "Which database?", "header": "DB", "multiSelect": false,
              "options": [{ "label": "Postgres", "description": "Relational" }, { "label": "SQLite" }, { "label": " " }] },
            { "question": "  ", "options": [] },
            { "question": "Which features?", "multiSelect": true, "options": [{ "label": "Auth", "description": "Sign in" }] },
        ]});
        let q = user_input_questions(&input);
        assert_eq!(q.len(), 2, "a question without text is dropped");
        assert_eq!(q[0].id, "Which database?");
        assert_eq!(q[0].header, "DB");
        assert_eq!(q[0].options.len(), 2, "an option without a label is dropped");
        assert_eq!(q[0].options[1].description, "SQLite", "the description defaults to the label");
        assert_eq!(q[1].header, "Question 3");
        assert_eq!((q[0].multi_select, q[1].multi_select), (Some(false), Some(true)));
        // As the client is sent them.
        assert_eq!(json!(q[1]), json!({ "id": "Which features?", "header": "Question 3", "question": "Which features?",
            "options": [{ "label": "Auth", "description": "Sign in" }], "multiSelect": true }));
    }

    #[test]
    fn answers_go_to_the_cli_as_one_string_per_question() {
        let given = json!({ "Which database?": "Postgres", "Which features?": ["Auth", "Billing"], "Other?": null });
        let a = cli_answers(given.as_object().unwrap());
        assert_eq!(a, json!({ "Which database?": "Postgres", "Which features?": "Auth, Billing", "Other?": "" }));
    }

    #[test]
    fn a_background_launch_receipt_is_not_an_answer() {
        assert!(super::is_background_launch("Async agent launched successfully. (internal)\nagentId: a1"));
        assert!(!super::is_background_launch("The files are a.txt and b.txt."));
    }

    #[test]
    fn every_edit_tool_shows_a_before_and_after() {
        assert_eq!(edit_texts("Edit", &json!({ "old_string": "a", "new_string": "b" })), Some(("a".into(), "b".into())));
        assert_eq!(edit_texts("Write", &json!({ "content": "new file" })), Some((String::new(), "new file".into())));
        let multi = json!({ "edits": [{ "old_string": "x", "new_string": "y" }, { "old_string": "p", "new_string": "q" }] });
        assert_eq!(edit_texts("MultiEdit", &multi), Some(("x\np".into(), "y\nq".into())));
        assert_eq!(edit_texts("NotebookEdit", &json!({ "notebook_path": "n.ipynb" })), None);
    }
}
