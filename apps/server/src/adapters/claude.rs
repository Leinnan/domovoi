//! Claude adapter over the `claude` CLI's stream-json protocol. One CLI
//! process per thread, kept alive between turns (as the Agent SDK does).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Claude CLI's stream-json messages, tool inputs and answer format")]

use super::turn::{Plan, Turn, TurnEnd, WindowUse, Work, decision_allows};
use super::{Control, TurnInput};
use crate::claude_cli::{ClaudeCli, CliEvent, LaunchOptions};
use crate::orchestration::commands::Answers;
use serde_json::{Value, json};
use std::collections::HashMap;
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
}

/// The answer to a request Claude makes while no turn is open (nobody can see it).
const IDLE_DENY: &str = "The user wasn't in the conversation to answer this. Ask again in your reply; they will answer in their next message.";

/// The CLI's permission mode: plan mode (look and propose, change nothing)
/// when the thread plans first, else the runtime mode's.
pub fn mode_for(input: &TurnInput) -> &'static str {
    if input.interaction_mode == "plan" { "plan" } else { permission_mode(&input.runtime_mode) }
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
            allowed_tools: if crate::mcp::enabled() {
                crate::mcp::read_only_tools().iter().map(|t| format!("mcp__{}__{t}", crate::mcp::SERVER_NAME)).collect()
            } else {
                vec![]
            },
        };
        let log_path = log_dir.join(format!("claude-{session_id}.ndjson"));
        let log = crate::logging::protocol_log(log_path);
        let (cli, events) = ClaudeCli::spawn(&opts, log).map_err(|e| format!("could not start claude: {e}"))?;
        let initialized = cli.initialize().await.map_err(|e| format!("claude initialize failed: {e}"))?;
        crate::skills::remember_descriptions(&initialized);
        Ok(Arc::new(ClaudeSession {
            cli, events: Mutex::new(events), session_id: session_id.to_owned(), cwd: input.cwd.clone(), permission_mode: Mutex::new(mode),
            reported_window: Default::default(), idle: Default::default(), broken: AtomicBool::new(false), echoes: AtomicBool::new(false),
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

    /// The thread's runtime mode changed. If Claude refuses the new mode,
    /// the next turn starts a fresh process in it.
    pub async fn set_runtime_mode(&self, runtime_mode: &str) {
        if !self.set_mode(permission_mode(runtime_mode)).await {
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
        if !self.set_mode(mode_for(&input)).await {
            // Still in the old mode: the next turn starts a fresh process.
            self.broken.store(true, Ordering::SeqCst);
        }
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
        let mut tool_inputs: HashMap<String, (String, Value)> = HashMap::new();
        let mut last_assistant_uuid: Option<String> = None;
        // Streamed blocks already completed by an `assistant` message.
        let mut finalized: std::collections::HashSet<String> = Default::default();
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
        // Subagents Claude runs in the background (tool use id -> description):
        // its reply ends Claude's turn but not the run, which stays open until
        // each reports back (`task_notification`); Claude then carries on by itself.
        let mut background: std::collections::BTreeMap<String, String> = Default::default();
        // Claude has replied and only background agents are working.
        let mut awaiting_background = false;
        // The last agent reported while Claude was between turns: Claude wakes
        // itself to act on it; if it doesn't by then, the run ends (t3code
        // CLAUDE_WAKE_TURN_START_DEADLINE).
        let mut wake_deadline: Option<tokio::time::Instant> = None;
        let pending_tasks = |background: &std::collections::BTreeMap<String, String>, task_ids: &HashMap<String, String>, subs: &super::subagents::Subagents| -> Vec<PendingBackgroundTask> {
            background.iter().map(|(tool, description)| PendingBackgroundTask {
                command: None,
                item_id: None,
                source_thread_id: None,
                task_id: task_ids.get(tool).cloned().unwrap_or_else(|| tool.clone()),
                description: Some(description.trim().to_owned()).filter(|d| !d.is_empty()),
                parent_task_id: None,
                native_item_id: Some(tool.clone()),
                awaited: Some(true),
                kind: Some(BackgroundTaskKind::Subagent),
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
                                let _ = self.cli.answer_permission(&cli_id, &req, decision_allows(&decision), None).await;
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
                            if let Some(markdown) = request["input"]["plan"].as_str().map(str::trim).filter(|p| !p.is_empty()) {
                                turn.plan(&item, Plan::Proposed { markdown: markdown.to_owned() }, true);
                            }
                            let _ = self.cli.answer_permission(&request_id, &request, false, Some(PLAN_CAPTURED)).await;
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
                            // Claude woke itself (a background agent reported back).
                            awaiting_background = false;
                            wake_deadline = None;
                            crate::providers::remember_claude_commands(&msg);
                            if crate::skills::remember_claude(&msg, &input.cwd) {
                                crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
                            }
                        }
                        "system" if msg["subtype"] == "task_started" => {
                            if let (Some(task), Some(tool)) = (msg["task_id"].as_str(), msg["tool_use_id"].as_str()) {
                                task_ids.insert(tool.to_owned(), task.to_owned());
                                if background.contains_key(tool) {
                                    turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
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
                                    blocks.insert(id, (kind, String::new()));
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
                                                let t = text.clone();
                                                let len = t.len();
                                                turn.item_streaming(&id, Work::Reasoning { text: t, streaming: true }, len);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        "assistant" => {
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
                                            subs.tool(&turn, &owner, &id, &name, &block["input"], None, "running");
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
                                        let text = block["thinking"].as_str().unwrap_or_default().to_owned();
                                        turn.item(&id, Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
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
                                        if name == "TodoWrite" && msg["parent_tool_use_id"].is_null() {
                                            turn.plan(&format!("todo:{id}"), Plan::Todo { steps: todo_steps(&block["input"]), explanation: None }, true);
                                        }
                                        record_tool(&mut turn, &id, &name, &block["input"], None, TurnItemStatus::Running);
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
                                let failed = block["is_error"] == true;
                                // Started in the background: the result is only a receipt
                                // (internal, not for the user); the agent reports later.
                                if subs.knows(&id) && !failed && is_background_launch(&output) {
                                    background.insert(id.clone(), tool_input["description"].as_str().unwrap_or_default().to_owned());
                                    subs.progress(&turn, &id, "Running in the background");
                                    turn.background_tasks(pending_tasks(&background, &task_ids, &subs));
                                    continue;
                                }
                                if subs.knows(&id) {
                                    subs.finish(&turn, &id, if failed { "failed" } else { "completed" }, Some(&output));
                                    continue;
                                }
                                if let Some(owner) = msg["parent_tool_use_id"].as_str().filter(|p| subs.knows(p)) {
                                    subs.tool(&turn, owner, &id, &name, &tool_input, Some(&output), if failed { "failed" } else { "completed" });
                                    continue;
                                }
                                record_tool(&mut turn, &id, &name, &tool_input, Some((&output, failed)), if failed { TurnItemStatus::Failed } else { TurnItemStatus::Completed });
                            }
                        }
                        // The bookkeeping result Claude sends for a task notice ends no turn.
                        "result" if awaiting_background && msg["subtype"] == "success" && msg["num_turns"] == 0 => {}
                        "rate_limit_event" => {
                            let info = &msg["rate_limit_info"];
                            if let Some(w) = crate::usage_limits::claude_window(info) {
                                crate::usage_limits::merge(input.model_selection.instance_id.as_str(), vec![w]);
                            }
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
                            // Claude replied but background agents still work: the run
                            // waits for them (and for what Claude does once they report).
                            if status == "completed" && !background.is_empty() {
                                awaiting_background = true;
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
                            return (turn, true);
                        }
                        _ => {}
                    },
                },
            }
        }
    }
}

/// How long Claude has to wake itself after its last background agent reports.
const WAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// A subagent's status from Claude's `task_notification` status.
pub fn task_status(status: Option<&str>) -> &'static str {
    match status {
        Some("stopped" | "killed") => "cancelled",
        Some("failed") => "failed",
        _ => "completed",
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
fn record_tool(turn: &mut Turn, id: &str, name: &str, input: &Value, result: Option<(&str, bool)>, status: TurnItemStatus) {
    let work = match name {
        "Bash" => Work::Command {
            input: input["command"].as_str().unwrap_or_default().to_owned(),
            output: result.map(|(out, _)| out.to_owned()),
            exit_code: result.map(|(_, failed)| i64::from(failed)),
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or("file");
            let file_name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.to_owned());
            let operation = if name == "Write" { "add" } else { "update" };
            let changes = vec![FileChangeDetail { operation: operation.into(), path: path.to_owned(), old_path: None, file_type: None, mime_type: None }];
            Work::FileChange { file_name, changes, texts: edit_texts(name, input), patch: None }
        }
        _ => Work::Tool { name: name.to_owned(), input: input.clone(), output: result.map(|(out, _)| Value::String(out.to_owned())), viewed_image_path: None },
    };
    turn.item(id, work, status);
}

#[cfg(test)]
mod tests {
    use super::{claude_token_usage, cli_answers, edit_texts, selected_window, user_input_questions};

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
