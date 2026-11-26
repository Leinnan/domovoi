//! Turns of the real agent CLIs, for what scripted stand-ins cannot show:
//! that the real `claude` and `codex` take what Domovoi sends them and send
//! back what Domovoi expects.
//!
//! Each test runs only when its provider's model is named, and spends real
//! tokens (the prompts are tiny):
//!
//! - `DOMOVOI_LIVE_CLAUDE_MODEL` (e.g. `claude-haiku-5-5`) for `claude`,
//! - `DOMOVOI_LIVE_CODEX_MODEL` for `codex`,
//! - `DOMOVOI_LIVE_ANTIGRAVITY_MODEL` (e.g. `gemini-3.8-flash`) for `agy`,
//!
//! found on PATH, or named by `DOMOVOI_CLAUDE_BINARY` / `DOMOVOI_CODEX_BINARY`.
//! The CLIs run as this user, signed in as they are. `just live claude` runs
//! them with `DOMOVOI_WIRE_OUT` set and decodes every frame with t3code's
//! schemas afterwards.
//!
//! A model decides for itself, so these ask for little and check what any
//! correct answer has: an approval asked for, a command's output, a word
//! given back.

use super::*;

/// A provider under test: its instance and the model to use.
pub struct Live {
    pub instance: &'static str,
    pub model: String,
}

/// The provider's model, if the test is to run.
fn live(instance: &'static str) -> Option<Live> {
    let var = match instance {
        "claudeAgent" => "DOMOVOI_LIVE_CLAUDE_MODEL",
        "codex" => "DOMOVOI_LIVE_CODEX_MODEL",
        "antigravity" => "DOMOVOI_LIVE_ANTIGRAVITY_MODEL",
        _ => return None,
    };
    let model = std::env::var(var).ok().filter(|m| !m.is_empty())?;
    Some(Live { instance, model })
}

/// The real CLI: named by `var`, else `name` on this process's PATH.
pub fn binary(var: &str, name: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    // `where` on Windows (its first answer: it lists every match).
    let finder = if cfg!(windows) { "where" } else { "/usr/bin/which" };
    let out = Command::new(finder).arg(name).output().unwrap();
    let found = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_owned();
    match (found.is_empty(), cfg!(windows)) {
        (false, _) => PathBuf::from(found),
        (true, true) => std::env::temp_dir().join("domovoi-no-such-cli").join(format!("{name}.exe")),
        (true, false) => PathBuf::from("/usr/bin/false"),
    }
}

/// A server running the real CLIs.
pub fn start_live(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("domovoi-provider-turns-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("live"), "").unwrap();
    for _ in 0..5 {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        for d in ["w", "out"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        if let Some(s) = spawn_in(dir.clone(), port) {
            return s;
        }
    }
    panic!("the server did not start");
}

/// How long a real turn may take.
const TURN: Duration = Duration::from_secs(240);

/// A signed-in socket on a live server whose provider is ready, with project
/// `p1` ("Domovoi Live") in a git repository.
async fn ready(server: &Server, p: &Live) -> (Ws, PathBuf) {
    let token = signed_in(server).await;
    let mut ws = socket(server, &token).await;
    if p.instance == "antigravity" {
        antigravity_signed_in(&mut ws).await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    loop {
        let r = call(&mut ws, &fresh_id("refresh"), "server.refreshProviders", json!({ "instanceId": p.instance })).await;
        let providers = r["exit"]["value"]["providers"].as_array().cloned().unwrap_or_default();
        let found = providers.iter().find(|x| x["instanceId"] == p.instance).unwrap_or_else(|| panic!("no {} provider: {r}", p.instance));
        if found["status"] == "ready" {
            break;
        }
        assert!(p.instance == "opencode" && tokio::time::Instant::now() < deadline
            && found["message"].as_str().is_some_and(|message| message.contains("service_starting")), "{found}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let workspace = workspace_repository(server);
    let r = call(&mut ws, &fresh_id("project"), "projects.mutate", json!({ "type": "project.create", "commandId": "c-project", "projectId": "p1", "title": "Domovoi Live",
        "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    (ws, workspace)
}

/// Launches thread `thread` on `p` with `text`.
async fn launch(ws: &mut Ws, p: &Live, thread: &str, text: &str, runtime: &str, interaction: &str) {
    let r = call(ws, &fresh_id("launch"), "orchestration.launchThread", json!({ "commandId": format!("launch-{thread}"), "creationSource": "web", "threadId": thread, "projectId": "p1",
        "title": "Live", "generateTitle": false, "modelSelection": { "instanceId": p.instance, "model": p.model },
        "runtimeMode": runtime, "interactionMode": interaction, "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": format!("{thread}-m1"), "text": text, "attachments": [] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
}

/// Sends `text` to `thread` as message `n`.
async fn send(ws: &mut Ws, thread: &str, n: u64, text: &str, extra: Value) {
    let mut cmd = json!({ "type": "message.dispatch", "commandId": format!("{thread}-say-{n}"), "threadId": thread, "createdBy": "user", "creationSource": "web",
        "dispatchMode": { "type": "queue_after_active" }, "messageId": format!("{thread}-m{n}"), "text": text, "attachments": [] });
    for (k, v) in extra.as_object().unwrap() {
        cmd[k.as_str()] = v.clone();
    }
    let r = call(ws, &fresh_id("say"), "orchestration.dispatchCommand", cmd).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
}

/// The id of run `ordinal` of the thread whose events these are.
fn run_id(all: &[Value], ordinal: u64) -> Option<Value> {
    all.iter().find(|e| e["type"] == "run.created" && e["payload"]["ordinal"] == ordinal).map(|e| e["payload"]["id"].clone())
}

fn status_of(all: &[Value], _thread: &str, ordinal: u64) -> Option<String> {
    let id = run_id(all, ordinal)?;
    all.iter().filter(|e| e["type"] == "run.updated" && e["payload"]["id"] == id).last().and_then(|e| e["payload"]["status"].as_str().map(str::to_owned))
}

/// Every message of the thread, for a failure to show.
fn transcript(all: &[Value]) -> String {
    let mut out = String::new();
    let mut seen = std::collections::BTreeMap::new();
    for e in all.iter().filter(|e| e["type"] == "message.updated") {
        let p = &e["payload"];
        seen.insert(p["id"].as_str().unwrap_or_default().to_owned(), format!("[{} {}] {}", p["role"], p["runId"], p["text"]));
    }
    for line in seen.values() {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Answers every request `thread` opens (`accept`, or the first option of
/// each question) until run `ordinal` ends; its events and how many requests
/// were answered. A run that fails fails the test with its error.
async fn settle(ws: &mut Ws, thread: &str, ordinal: u64) -> (Vec<Value>, usize) {
    let deadline = tokio::time::Instant::now() + TURN;
    let mut answered = std::collections::HashSet::new();
    loop {
        let all = events(ws, &fresh_id("live"), thread).await;
        for req in all.iter().filter(|e| e["type"] == "runtime-request.updated") {
            let p = &req["payload"];
            let id = p["id"].as_str().unwrap().to_owned();
            if p["status"] != "pending" || answered.contains(&id) {
                continue;
            }
            let mut respond = json!({ "type": "runtime-request.respond", "commandId": format!("answer-{thread}-{id}"), "threadId": thread, "requestId": id });
            if p["kind"] == "user_input" {
                let item = all.iter().filter(|e| e["type"] == "turn-item.updated" && e["payload"]["requestId"] == p["id"]).last();
                let mut answers = serde_json::Map::new();
                for q in item.and_then(|i| i["payload"]["questions"].as_array()).cloned().unwrap_or_default() {
                    let first = q["options"][0]["label"].as_str().unwrap_or("yes").to_owned();
                    answers.insert(q["id"].as_str().unwrap_or_default().to_owned(), json!([first]));
                }
                respond["answers"] = Value::Object(answers);
            } else {
                respond["decision"] = json!("accept");
            }
            let r = call(ws, &fresh_id("answer"), "orchestration.dispatchCommand", respond).await;
            assert_eq!(r["exit"]["_tag"], "Success", "{r}");
            answered.insert(id);
        }
        match status_of(&all, thread, ordinal).as_deref() {
            Some("completed" | "interrupted" | "rolled_back") => return (all, answered.len()),
            Some("failed") => {
                let error = all.iter().filter(|e| e["type"] == "turn-item.updated" && e["payload"]["type"] == "error").last().map(|e| e["payload"].clone());
                panic!("run {ordinal} of {thread} failed: {error:?}");
            }
            _ => {}
        }
        assert!(tokio::time::Instant::now() < deadline, "run {ordinal} of {thread} did not end: {:?}", status_of(&all, thread, ordinal));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The text of the last assistant message of run `ordinal`.
fn reply(all: &[Value], _thread: &str, ordinal: u64) -> String {
    let Some(run) = run_id(all, ordinal) else { return String::new() };
    all.iter()
        .filter(|e| e["type"] == "message.updated" && e["payload"]["role"] == "assistant" && e["payload"]["runId"] == run)
        .last()
        .and_then(|e| e["payload"]["text"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn items<'a>(all: &'a [Value], ty: &str) -> Vec<&'a Value> {
    all.iter().filter(|e| e["type"] == "turn-item.updated" && e["payload"]["type"] == ty).map(|e| &e["payload"]).collect()
}

/// One test per provider for each scenario.
macro_rules! live_tests {
    ($($name:ident),* $(,)?) => {
        mod claude { $(#[tokio::test(flavor = "multi_thread")] async fn $name() { if let Some(p) = super::live("claudeAgent") { super::$name(p).await } })* }
        mod codex { $(#[tokio::test(flavor = "multi_thread")] async fn $name() { if let Some(p) = super::live("codex") { super::$name(p).await } })* }
    };
}

live_tests!(worktree_handoff, command_after_approval, plan_then_implement, steer_queue_and_stop, native_fork, subagent, agent_tools, delegated_task, text_generation, restart_continuation, image_attachment, scheduled_run);

/// A command asks first, runs once allowed, and its output comes back; the
/// context used is reported.
async fn command_after_approval(p: Live) {
    let server = start_live(&format!("command-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
        // A command that writes: CLIs run read-only ones (a bare `echo`) without asking.
    launch(&mut ws, &p, "t1", "Run exactly this shell command: mkdir -p live-dir && echo hello-domovoi. Then reply with its output and nothing else.", "approval-required", "default").await;
    let (all, answered) = settle(&mut ws, "t1", 1).await;
    assert!(answered > 0, "the command asked first: {:?}", item_types(&all));
    let cmd = items(&all, "command_execution").into_iter().find(|c| c["output"].as_str().unwrap_or("").contains("hello-domovoi"));
    assert!(cmd.is_some(), "the command ran: {:?}", items(&all, "command_execution"));
    assert!(reply(&all, "t1", 1).contains("hello-domovoi"), "{}", reply(&all, "t1", 1));
    let pt = all.iter().filter(|e| e["type"] == "provider-thread.updated").last().expect("a provider thread");
    assert!(pt["payload"]["contextUsage"]["usedTokens"].as_u64().unwrap_or(0) > 0, "context used is reported: {pt}");
}

/// Plan mode proposes a plan without touching files; implementing it writes
/// the file and completes the plan.
async fn plan_then_implement(p: Live) {
    let server = start_live(&format!("plan-{}", p.instance));
    let (mut ws, workspace) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Make a short plan (two steps) to create a file named plan.txt containing the word planned. Only plan; do not create it yet.", "full-access", "plan").await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    assert!(!workspace.join("plan.txt").exists(), "plan mode wrote nothing");
    let proposed = items(&all, "proposed_plan").pop().cloned().unwrap_or_else(|| panic!("a proposed plan: {:?}", item_types(&all)));
    let r = call(&mut ws, &fresh_id("mode"), "orchestration.dispatchCommand", json!({ "type": "thread.interaction-mode.set", "commandId": "mode-1", "threadId": "t1", "interactionMode": "default" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    send(&mut ws, "t1", 2, "Implement the plan.", json!({ "sourcePlanRef": { "threadId": "t1", "planId": proposed["planId"] } })).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    let written = std::fs::read_to_string(workspace.join("plan.txt")).unwrap_or_default();
    assert!(written.contains("planned"), "the plan was implemented: {written:?}");
    let plan = all.iter().filter(|e| e["type"] == "plan.updated" && e["payload"]["id"] == proposed["planId"]).last().unwrap();
    assert_eq!(plan["payload"]["status"], "completed");
}

/// A steer reaches the running turn, a queued message waits its turn, and a
/// stop interrupts. On macOS the machine is held awake while it works.
async fn steer_queue_and_stop(p: Live) {
    let server = start_live(&format!("steer-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Run the shell command `sleep 20`, then reply with the word FIRST.", "full-access", "default").await;
    until_within(TURN, &mut ws, "t1", "the sleep to start", |all| items(all, "command_execution").iter().any(|c| c["input"].as_str().unwrap_or("").contains("sleep"))).await;
    if cfg!(target_os = "macos") {
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(held_awake(&server), "held awake while the agent works");
    }
    send(&mut ws, "t1", 2, "Also put the word SECOND in your reply.", json!({ "dispatchMode": { "type": "steer_active", "targetRunId": "run:thread:t1:ordinal:1" } })).await;
    send(&mut ws, "t1", 3, "Reply with the word QUEUED.", json!({})).await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    let steer = all.iter().find(|e| e["type"] == "message.updated" && e["payload"]["text"] == "Also put the word SECOND in your reply.").expect("the steer message");
    assert_eq!(steer["payload"]["runId"], "run:thread:t1:ordinal:1", "the steer belongs to the running turn");
    assert!(reply(&all, "t1", 1).contains("SECOND"), "the steer reached the model:\n{}", transcript(&all));
    // The queued message ran after it, as run 2 (the steer made no run of its own).
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert!(reply(&all, "t1", 2).contains("QUEUED"), "{}", transcript(&all));

    send(&mut ws, "t1", 4, "Run the shell command `sleep 60`, then reply with DONE.", json!({})).await;
    until_within(TURN, &mut ws, "t1", "the second sleep", |all| status_of(all, "t1", 3).as_deref() == Some("running") && items(all, "command_execution").iter().filter(|c| c["input"].as_str().unwrap_or("").contains("sleep 60")).count() > 0).await;
    let r = call(&mut ws, &fresh_id("stop"), "orchestration.dispatchCommand", json!({ "type": "thread.stop", "commandId": "stop-1", "threadId": "t1" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let (all, _) = settle(&mut ws, "t1", 3).await;
    assert_eq!(status_of(&all, "t1", 3).as_deref(), Some("interrupted"));
}

/// A fork remembers what its source was told, through the provider's own
/// fork (no text handoff).
async fn native_fork(p: Live) {
    let server = start_live(&format!("fork-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Remember the codeword PAPAYA. Reply only with OK.", "full-access", "default").await;
    settle(&mut ws, "t1", 1).await;
    let r = call(&mut ws, &fresh_id("fork"), "orchestration.dispatchCommand", json!({ "type": "thread.fork", "commandId": "fork-1", "sourceThreadId": "t1", "targetThreadId": "t2",
        "sourcePoint": { "type": "latest_stable" }, "createdBy": "user", "creationSource": "web" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    send(&mut ws, "t2", 1, "What was the codeword? Reply with just the word.", json!({})).await;
    let (all, _) = settle(&mut ws, "t2", 1).await;
    assert!(reply(&all, "t2", 1).to_uppercase().contains("PAPAYA"), "the fork remembers: {}", reply(&all, "t2", 1));
    assert!(items(&all, "handoff").is_empty(), "forked natively, not handed over as text");
    // The source goes on unchanged.
    send(&mut ws, "t1", 2, "Reply with the word STILL.", json!({})).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert!(reply(&all, "t1", 2).contains("STILL"));
}

/// A subagent's work goes to its own child thread.
async fn subagent(p: Live) {
    let server = start_live(&format!("subagent-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    let ask = if p.instance == "codex" {
        "Spawn one subagent whose only task is to reply with the word KIWI. Wait for it, then tell me what it said."
    } else if p.instance == "antigravity" {
        "Use your subagent tool (invoke_subagent) to start one subagent whose only task is to reply with the word KIWI. Then tell me what it said."
    } else {
        "Use the Task tool to start one subagent whose only task is to reply with the word KIWI. Then tell me what it said."
    };
    launch(&mut ws, &p, "t1", ask, "full-access", "default").await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    let Some(sub) = all.iter().filter(|e| e["type"] == "subagent.updated").last().map(|e| e["payload"].clone()) else {
        // Codex spawns agents only with its collaboration feature on.
        assert_eq!(p.instance, "codex", "Claude started no subagent: {:?}", item_types(&all));
        eprintln!("codex started no subagent (is its multi-agent feature on?)");
        return;
    };
    assert_eq!(sub["status"], "completed", "{sub}");
    let child = sub["childThreadId"].as_str().expect("a child thread").to_owned();
    let work = events(&mut ws, &fresh_id("child"), &child).await;
    let created = work.iter().find(|e| e["type"] == "thread.created").unwrap();
    assert_eq!(created["payload"]["lineage"]["relationshipToParent"], "subagent");
    assert!(reply(&all, "t1", 1).contains("KIWI"), "{}", reply(&all, "t1", 1));
}

/// The agent calls a Domovoi MCP tool and uses what it answered.
async fn agent_tools(p: Live) {
    let server = start_live(&format!("tools-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Call the project_info tool, then reply with the project's title and nothing else.", "full-access", "default").await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    let called = all.iter().any(|e| e["type"] == "turn-item.updated" && e["payload"].to_string().contains("project_info"));
    assert!(called, "the tool was called: {:?}", item_types(&all));
    assert!(reply(&all, "t1", 1).contains("Domovoi Live"), "{}", reply(&all, "t1", 1));
}

/// A delegated task runs in a child thread and its answer comes back.
async fn delegated_task(p: Live) {
    let server = start_live(&format!("delegate-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Use the delegate_task tool to give another agent this task: reply with the word MANGO. Wait for it with thread_wait, then tell me what it replied.", "full-access", "default").await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    assert!(reply(&all, "t1", 1).contains("MANGO"), "{}", reply(&all, "t1", 1));
    let r = call(&mut ws, &fresh_id("shell"), "orchestration.getThreadProjection", json!({ "threadId": "t1" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
}

/// Runs a stacked git action; the `action_finished` event.
async fn stacked(ws: &mut Ws, cwd: &str, action: &str) -> Value {
    let id = fresh_id("stacked");
    let request = json!({ "_tag": "Request", "id": id, "tag": "git.runStackedAction", "payload": { "actionId": id, "cwd": cwd, "action": action }, "headers": [] });
    record(&request);
    ws.send(Message::Text(request.to_string().into())).await.unwrap();
    let mut finished = Value::Null;
    loop {
        let t = match tokio::time::timeout(TURN, ws.next()).await.expect("the action finishes") {
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(_)) => continue,
            _ => panic!("socket closed"),
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["requestId"] != json!(id) {
            continue;
        }
        record(&v);
        if v["_tag"] == "Chunk" {
            ws.send(Message::Text(json!({ "_tag": "Ack", "requestId": id }).to_string().into())).await.unwrap();
            for c in v["values"].as_array().unwrap() {
                if c["kind"] == "action_finished" {
                    finished = c.clone();
                }
            }
        }
        if v["_tag"] == "Exit" {
            return finished;
        }
    }
}

/// The provider's model names the thread and its branch and writes a commit
/// message.
async fn text_generation(p: Live) {
    let server = start_live(&format!("textgen-{}", p.instance));
    let (mut ws, workspace) = ready(&server, &p).await;
    git(&workspace, &["config", "user.name", "t"]);
    git(&workspace, &["config", "user.email", "t@t"]);
    let r = call(&mut ws, &fresh_id("settings"), "server.updateSettings", json!({ "patch": { "textGenerationModelSelection": { "instanceId": p.instance, "model": p.model } } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let seed = "add a search bar to the header";
    let r = call(&mut ws, &fresh_id("launch"), "orchestration.launchThread", json!({ "commandId": "launch-t1", "creationSource": "web", "threadId": "t1", "projectId": "p1",
        "title": seed, "generateTitle": true, "modelSelection": { "instanceId": p.instance, "model": p.model },
        "runtimeMode": "full-access", "interactionMode": "default", "workspaceStrategy": { "type": "worktree", "baseRef": "main" },
        "initialMessage": { "messageId": "t1-m1", "text": format!("We will {seed} later. For now reply only with OK and change nothing."), "attachments": [] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let worktree = r["exit"]["value"]["projection"]["thread"]["worktreePath"].as_str().unwrap().to_owned();
    let all = until_within(TURN, &mut ws, "t1", "the title and branch", |all| {
        // A new worktree's branch is `t3/<8 hex digits>` until it is named.
        let temporary = |b: &str| b.strip_prefix("t3/").is_some_and(|h| h.len() == 8 && h.chars().all(|c| c.is_ascii_hexdigit()));
        last(all, "thread.metadata-updated").is_some_and(|t| t["lastTitleRegeneration"]["status"] == "renamed" && t["branch"].as_str().is_some_and(|b| !temporary(b)))
    })
    .await;
    let t = last(&all, "thread.metadata-updated").unwrap();
    assert_ne!(t["title"], seed, "a written title: {t}");
    eprintln!("title {:?}, branch {:?}", t["title"], t["branch"]);
    settle(&mut ws, "t1", 1).await;

    std::fs::write(std::path::Path::new(&worktree).join("search.txt"), "search bar\n").unwrap();
    let finished = stacked(&mut ws, &worktree, "commit").await;
    let subject = finished["result"]["commit"]["subject"].as_str().unwrap_or_default().to_owned();
    assert!(!subject.is_empty(), "a written commit message: {finished}");
    eprintln!("commit {subject:?}");
}

/// A turn cut off by a deliberate stop of the server is continued after it
/// starts again.
async fn restart_continuation(p: Live) {
    let server = start_live(&format!("restart-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Run the shell command `sleep 15`, then reply with the word DONE.", "full-access", "default").await;
    until_within(TURN, &mut ws, "t1", "the sleep to start", |all| items(all, "command_execution").iter().any(|c| c["input"].as_str().unwrap_or("").contains("sleep"))).await;
    drop(ws);
    let server = restart(server);
    let token = signed_in(&server).await;
    let mut ws = socket(&server, &token).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    let run = all.iter().find(|e| e["type"] == "run.created" && e["payload"]["ordinal"] == 2).unwrap();
    assert_eq!(run["payload"]["restartContinuationOfRunId"], "run:thread:t1:ordinal:1");
    assert_eq!(status_of(&all, "t1", 1).as_deref(), Some("interrupted"));
}

/// An attached image reaches the model.
async fn image_attachment(p: Live) {
    let server = start_live(&format!("image-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    let size = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, PNG).unwrap().len();
    let r = call(&mut ws, &fresh_id("persist"), "assets.persistChatAttachments", json!({ "threadId": "t1", "messageId": "t1-m1",
        "attachments": [{ "type": "image", "name": "dot.png", "mimeType": "image/png", "sizeBytes": size, "dataUrl": format!("data:image/png;base64,{PNG}") }] })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, &fresh_id("launch"), "orchestration.launchThread", json!({ "commandId": "launch-t1", "creationSource": "web", "threadId": "t1", "projectId": "p1",
        "title": "Image", "generateTitle": false, "modelSelection": { "instanceId": p.instance, "model": p.model },
        "runtimeMode": "full-access", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": "t1-m1", "text": "How many images did I attach? Reply with just the number as a digit.", "attachments": r["exit"]["value"]["attachments"] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let (all, _) = settle(&mut ws, "t1", 1).await;
    assert!(reply(&all, "t1", 1).contains('1'), "{}", reply(&all, "t1", 1));
}

/// A scheduled task run now starts its thread and the turn completes.
async fn scheduled_run(p: Live) {
    let server = start_live(&format!("scheduled-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    let r = call(&mut ws, &fresh_id("upsert"), "scheduledTasks.upsert", json!({ "id": "task-1", "title": "Live", "prompt": "Reply with the word SCHEDULED.", "enabled": true,
        "schedule": { "type": "interval", "everyMs": 86_400_000 }, "projectId": "p1", "threadMode": "continue",
        "workspaceStrategy": { "type": "root" }, "modelSelection": { "instanceId": p.instance, "model": p.model }, "runtimeMode": "full-access", "interactionMode": "default" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, &fresh_id("run"), "scheduledTasks.runNow", json!({ "id": "task-1" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    // A task that continues its thread is bound to the one its first run made.
    let thread = r["exit"]["value"]["task"]["threadId"].as_str().unwrap_or_else(|| panic!("the run's thread: {r}")).to_owned();
    let (all, _) = settle(&mut ws, &thread, 1).await;
    assert!(reply(&all, &thread, 1).contains("SCHEDULED"), "{}", reply(&all, &thread, 1));
}

/// With both providers named: switching a thread from Claude to Codex hands
/// the conversation over as text, and Codex knows what Claude was told.
#[tokio::test(flavor = "multi_thread")]
async fn a_thread_switched_to_another_provider_keeps_its_conversation() {
    let (Some(claude), Some(codex)) = (live("claudeAgent"), live("codex")) else { return };
    let server = start_live("handoff");
    let (mut ws, _) = ready(&server, &claude).await;
    let r = call(&mut ws, &fresh_id("refresh"), "server.refreshProviders", json!({ "instanceId": "codex" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    launch(&mut ws, &claude, "t1", "Remember the codeword PAPAYA. Reply only with OK.", "full-access", "default").await;
    settle(&mut ws, "t1", 1).await;
    send(&mut ws, "t1", 2, "What was the codeword? Reply with just the word.", json!({ "modelSelection": { "instanceId": codex.instance, "model": codex.model } })).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert!(!items(&all, "handoff").is_empty(), "a handoff item: {:?}", item_types(&all));
    assert!(reply(&all, "t1", 2).to_uppercase().contains("PAPAYA"), "Codex knows:\n{}", transcript(&all));
}

/// The agent moves its thread into a worktree with `worktree_handoff`;
/// the continuation runs there, in a new session that has the conversation.
async fn worktree_handoff(p: Live) {
    let server = start_live(&format!("handoff-{}", p.instance));
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Remember the codeword PAPAYA. Then call the worktree_handoff tool with branch \"live-handoff\" and continuationPrompt \"Reply with the codeword I gave you, then run pwd.\". Then end your turn.", "full-access", "default").await;
    settle(&mut ws, "t1", 1).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert!(reply(&all, "t1", 2).to_uppercase().contains("PAPAYA"), "the conversation went along:\n{}", transcript(&all));
    let pwd = items(&all, "command_execution").into_iter().filter_map(|c| c["output"].as_str()).any(|o| o.contains("live-handoff") || o.contains("worktrees"));
    assert!(pwd, "the continuation ran in the worktree: {:?}", items(&all, "command_execution"));
}

/// With `DOMOVOI_LIVE_ACP_COMMAND` (e.g. `opencode acp`) and
/// `DOMOVOI_LIVE_ACP_MODEL` (one of its models, e.g. `ollama/qwen3.5:4b`): a
/// real ACP agent added as a local command is probed, runs a command after
/// an approval, and replies.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_acp_agent_runs_a_turn() {
    let (Ok(command), Ok(model)) = (std::env::var("DOMOVOI_LIVE_ACP_COMMAND"), std::env::var("DOMOVOI_LIVE_ACP_MODEL")) else { return };
    let mut words = command.split_whitespace();
    let program = binary("DOMOVOI_LIVE_ACP_BINARY", words.next().unwrap());
    let args: Vec<&str> = words.collect();
    let server = start_live("acp");
    let token = signed_in(&server).await;
    let mut ws = socket(&server, &token).await;
    let r = call(&mut ws, &fresh_id("s"), "server.updateSettings", json!({ "patch": {}, "providerInstanceMutation": { "operation": "upsert", "instanceId": "live-acp",
        "instance": { "driver": "acpRegistry", "config": { "source": "local", "commandPath": program, "commandArgs": args } } } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let p = Live { instance: "live-acp", model };
    let (mut ws2, _) = ready(&server, &p).await;
    drop(ws);
    let r = call(&mut ws2, &fresh_id("p"), "server.refreshProviders", json!({ "instanceId": "live-acp" })).await;
    let provider = r["exit"]["value"]["providers"].as_array().unwrap().iter().find(|x| x["instanceId"] == "live-acp").cloned().unwrap();
    eprintln!("{} {} with {} models", provider["displayName"], provider["version"], provider["models"].as_array().map_or(0, Vec::len));
    launch(&mut ws2, &p, "t1", "Use your shell tool to run exactly this command: mkdir -p acp-dir && echo hello-acp. Then reply with its output and nothing else.", "approval-required", "default").await;
    let (all, answered) = settle(&mut ws2, "t1", 1).await;
    eprintln!("answered {answered} requests; items {:?}", item_types(&all));
    let said = transcript(&all);
    assert!(said.contains("hello-acp") || items(&all, "command_execution").iter().any(|c| c["output"].as_str().unwrap_or("").contains("hello-acp")), "{said}");
}

/// With `DOMOVOI_LIVE_ACP_REGISTRY_AGENT` (an agent id of the real ACP
/// registry, e.g. `pi-acp`): the registry is searched, the agent installed,
/// and an instance naming it starts and answers the probe.
#[tokio::test(flavor = "multi_thread")]
async fn a_registry_agent_installs_and_starts() {
    let Ok(agent) = std::env::var("DOMOVOI_LIVE_ACP_REGISTRY_AGENT") else { return };
    let server = start_live("acp-registry");
    let token = signed_in(&server).await;
    let mut ws = socket(&server, &token).await;
    let r = call(&mut ws, &fresh_id("q"), "server.searchAcpRegistry", json!({ "query": "" })).await;
    let agents = r["exit"]["value"]["agents"].as_array().cloned().unwrap_or_else(|| panic!("{r}"));
    eprintln!("{} agents run here", agents.len());
    assert!(agents.iter().any(|a| a["id"] == json!(agent)), "{agent} is listed");
    let r = call(&mut ws, &fresh_id("prep"), "server.prepareAcpRegistryAgent", json!({ "agentId": agent })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    eprintln!("prepared {}", r["exit"]["value"]);
    let r = call(&mut ws, &fresh_id("s"), "server.updateSettings", json!({ "patch": {}, "providerInstanceMutation": { "operation": "upsert", "instanceId": "registry-agent",
        "instance": { "driver": "acpRegistry", "config": { "source": "registry", "agentId": agent } } } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, &fresh_id("p"), "server.refreshProviders", json!({ "instanceId": "registry-agent" })).await;
    let p = r["exit"]["value"]["providers"].as_array().unwrap().iter().find(|x| x["instanceId"] == "registry-agent").cloned().unwrap();
    eprintln!("{} {} {}: {}", p["displayName"], p["version"], p["status"], p["message"]);
    assert_eq!(p["installed"], true, "it started and initialized: {p}");
    let r = call(&mut ws, &fresh_id("u"), "server.uninstallAcpRegistryManagedBinary", json!({ "agentId": agent })).await;
    assert_eq!(r["exit"]["_tag"], "Failure", "an instance still uses it: {r}");
}

/// Antigravity's fresh profile signed in: through the machine's shared agy
/// sign-in it finishes by itself; when Google wants a browser, the test
/// prints the link and stops.
async fn antigravity_signed_in(ws: &mut Ws) {
    let r = call(ws, &fresh_id("p"), "server.refreshProviders", json!({ "instanceId": "antigravity" })).await;
    let p = r["exit"]["value"]["providers"].as_array().unwrap().iter().find(|x| x["instanceId"] == "antigravity").cloned().expect("agy found");
    if p["status"] == "ready" {
        return;
    }
    let r = call(ws, &fresh_id("a"), "provider.auth.start", json!({ "instanceId": "antigravity" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let done = auth_state(ws, "antigravity", "the sign-in to end or wait for a browser", |s| matches!(s["phase"].as_str(), Some("succeeded" | "failed" | "waiting"))).await;
    if done["phase"] == "waiting" {
        panic!("Google needs a browser sign-in: open {} (or run `agy` and sign in), then run again", done["authorizationUrl"]);
    }
    assert_eq!(done["phase"], "succeeded", "{done}");
}

/// Keeps a live server's protocol logs of Antigravity, and the agent's own
/// (its sign-in and token handling), under
/// `target/live-antigravity-logs/<name>` as the server ends, passed or
/// failed: to compare with the scripted agy, and so a lost sign-in leaves
/// the agent's account of it.
pub fn keep_antigravity_logs_of(dir: &std::path::Path, name: &str) {
    let agent_logs = dir.join("home/userdata/providers/antigravity");
    if !dir.join("live").exists() || !agent_logs.exists() {
        return;
    }
    let to = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/live-antigravity-logs").join(name);
    let _ = std::fs::remove_dir_all(&to);
    if std::fs::create_dir_all(&to).is_err() {
        return;
    }
    for e in std::fs::read_dir(dir.join("home/userdata/logs/provider")).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with("antigravity-") {
            let _ = std::fs::copy(e.path(), to.join(e.file_name()));
        }
    }
    // The agent's own logs (its sign-in and token handling).
    for profile in std::fs::read_dir(&agent_logs).into_iter().flatten().flatten() {
        for e in std::fs::read_dir(profile.path().join("antigravity-acp/log")).into_iter().flatten().flatten() {
            let _ = std::fs::copy(e.path(), to.join(e.file_name()));
        }
    }
}

/// The scenarios Antigravity shares with the others (it has no steering,
/// native forks or plan toggle), and its own.
mod antigravity {
    macro_rules! scenarios {
        ($($name:ident),* $(,)?) => {
            $(#[tokio::test(flavor = "multi_thread")] async fn $name() { if let Some(p) = super::live("antigravity") { super::$name(p).await } })*
        };
    }
    scenarios!(command_after_approval, subagent, agent_tools, text_generation, restart_continuation, image_attachment, scheduled_run, edits_and_reads, stop_then_resume, subagent_batch);
}

/// Antigravity's edit is shown at its line, and a file it reads through the
/// client completes.
async fn edits_and_reads(p: Live) {
    let server = start_live("antigravity-edits");
    let (mut ws, workspace) = ready(&server, &p).await;
    std::fs::write(workspace.join("notes.txt"), "alpha\nbravo\ncharlie\ndelta\n").unwrap();
    launch(&mut ws, &p, "t1", "In the file notes.txt, replace the line `charlie` with `CHARLIE` using your file editing tool (not a shell command). Then reply with DONE.", "approval-required", "default").await;
    let (all, answered) = settle(&mut ws, "t1", 1).await;
    eprintln!("answered {answered}; items {:?}", item_types(&all));
    assert_eq!(std::fs::read_to_string(workspace.join("notes.txt")).unwrap(), "alpha\nbravo\nCHARLIE\ndelta\n");
    let edit = items(&all, "file_change").into_iter().last().cloned().unwrap_or_else(|| panic!("an edit: {:?}", item_types(&all)));
    eprintln!("edit: {edit}");
    let diff = edit["diffStr"].as_str().unwrap_or_default();
    assert!(diff.contains(" bravo\n-charlie\n+CHARLIE\n delta"), "at its place in the file: {edit}");
    send(&mut ws, "t1", 2, "Read notes.txt with your file viewing tool and reply with its last line only.", json!({})).await;
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert!(reply(&all, "t1", 2).contains("delta"), "{}", transcript(&all));
    let open: Vec<_> = items(&all, "dynamic_tool_call").into_iter().filter(|t| t["status"] != "completed").map(|t| t["title"].clone()).collect();
    assert!(open.is_empty(), "every tool call ended: {open:?}");
}

/// A stopped turn ends, and after a server restart the thread's session is
/// resumed (the agent still knows what it was told).
async fn stop_then_resume(p: Live) {
    let server = start_live("antigravity-resume");
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Remember the codeword PAPAYA. Reply only with OK.", "full-access", "default").await;
    settle(&mut ws, "t1", 1).await;
    send(&mut ws, "t1", 2, "Run the shell command `sleep 60`, then reply with DONE.", json!({})).await;
    until_within(TURN, &mut ws, "t1", "the sleep", |all| items(all, "command_execution").iter().any(|c| c["input"].as_str().unwrap_or("").contains("sleep 60"))).await;
    let r = call(&mut ws, &fresh_id("stop"), "orchestration.dispatchCommand", json!({ "type": "thread.stop", "commandId": "stop-1", "threadId": "t1" })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let (all, _) = settle(&mut ws, "t1", 2).await;
    assert_eq!(status_of(&all, "t1", 2).as_deref(), Some("interrupted"));
    drop(ws);
    let server = restart(server);
    let token = signed_in(&server).await;
    let mut ws = socket(&server, &token).await;
    send(&mut ws, "t1", 3, "What was the codeword? Reply with just the word.", json!({})).await;
    let (all, _) = settle(&mut ws, "t1", 3).await;
    assert!(reply(&all, "t1", 3).to_uppercase().contains("PAPAYA"), "the resumed session remembers:\n{}", transcript(&all));
    let logs = std::fs::read_dir(server.dir.join("home/userdata/logs/provider")).unwrap().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap_or_default()).collect::<String>();
    assert!(logs.contains("\"session/resume\""), "the session was resumed, not started anew");
}

/// A subagent batch: its child shows read-only through
/// `orchestration.getNativeChildWork`, read from the parent's session log.
async fn subagent_batch(p: Live) {
    let server = start_live("antigravity-batch");
    let (mut ws, _) = ready(&server, &p).await;
    launch(&mut ws, &p, "t1", "Use your subagent tool (invoke_subagent) to start exactly one subagent with the role `tester` whose only task is to reply with the word KIWI. Then tell me what it said.", "full-access", "default").await;
    let (all, _) = settle(&mut ws, "t1", 1).await;
    let sub = all.iter().filter(|e| e["type"] == "subagent.updated").last().map(|e| e["payload"].clone()).unwrap_or_else(|| panic!("no subagent: {:?}\n{}", item_types(&all), transcript(&all)));
    eprintln!("batch: {sub}");
    let child = sub["childThreadId"].as_str().expect("a child thread").to_owned();
    let r = call(&mut ws, &fresh_id("cw"), "orchestration.getNativeChildWork", json!({ "threadId": child })).await;
    eprintln!("child work: {}", r["exit"]["value"]);
    let children = r["exit"]["value"]["children"].as_array().cloned().unwrap_or_default();
    assert_eq!(children.len(), 1, "the batch's child, proved by the session log: {r}");
    assert!(children[0]["steps"].as_array().is_some_and(|s| !s.is_empty()), "its transcript: {r}");
}

/// Opt-in model behavior probe. Use DOMOVOI_SERVER_BIN to run the same inputs
/// against a baseline build and compare the number of structured questions.
#[tokio::test(flavor = "multi_thread")]
async fn question_frequency_smoke() {
    for (instance, variable) in [
        ("antigravity", "DOMOVOI_QUESTION_SMOKE_ANTIGRAVITY_MODEL"),
        ("opencode", "DOMOVOI_QUESTION_SMOKE_OPENCODE_MODEL"),
    ] {
        let Ok(model) = std::env::var(variable) else { continue };
        let provider = Live { instance, model };
        let server = start_live(&format!("question-smoke-{instance}"));
        let (mut ws, _) = ready(&server, &provider).await;
        for (thread, interaction, text) in [
            ("ambiguous-plan", "plan", "Plan an offline reading-list app for a small newsroom. Editors share iPads, reporters use their own phones, and we haven't agreed which rollout audience should come first. Use only the brief above and leave the workspace untouched."),
            ("consequential-choice", "default", "Draft release copy for our new offline mode. The team has two audiences in mind, field reporters or editors, and only one goes in the first announcement. I haven't chosen the audience yet. Use only the brief above and leave the workspace untouched."),
            ("fully-specified", "default", "Write one sentence of release copy for field reporters: offline mode keeps their saved articles readable without a connection. Use plain language. Use only the brief above and leave the workspace untouched."),
        ] {
            launch(&mut ws, &provider, thread, text, "approval-required", interaction).await;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
            let mut answered_approvals = std::collections::HashSet::new();
            loop {
                let all = events(&mut ws, &fresh_id("question-smoke"), thread).await;
                let question = super::latest_question(&all).filter(|question| question["status"] == "pending");
                let status = status_of(&all, thread, 1);
                if let Some(question) = question {
                    println!("question-smoke {instance} {thread}: card");
                    let r = call(&mut ws, &fresh_id("dismiss"), "orchestration.dispatchCommand", json!({
                        "type": "thread.user-input.dismiss", "commandId": format!("dismiss-{thread}"), "threadId": thread, "requestId": question["id"]
                    })).await;
                    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
                    break;
                }
                for event in all.iter().filter(|event| event["type"] == "turn-item.updated" && event["payload"]["type"] == "approval_request") {
                    let request = &event["payload"];
                    let Some(id) = request["requestId"].as_str() else { continue };
                    let pending = all.iter().rev().find(|event| event["type"] == "runtime-request.updated" && event["payload"]["id"] == id);
                    if !pending.is_some_and(|event| event["payload"]["status"] == "pending") || !answered_approvals.insert(id.to_owned()) {
                        continue;
                    }
                    let decision = if matches!(request["prompt"].as_str(), Some("ls -la" | "pwd" | "git status --short")) { "accept" } else { "decline" };
                    let r = call(&mut ws, &fresh_id("approval"), "orchestration.dispatchCommand", json!({
                        "type": "runtime-request.respond", "commandId": format!("respond-{id}"), "threadId": thread, "requestId": id, "decision": decision
                    })).await;
                    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
                }
                if matches!(status.as_deref(), Some("completed" | "failed" | "interrupted")) {
                    println!("question-smoke {instance} {thread}: {}", status.unwrap());
                    if let Some(failure) = all.iter().rev().find(|event| event["payload"]["type"] == "error") {
                        eprintln!("Provider error: {}", failure["payload"]["failure"]["message"].as_str().unwrap_or("unspecified"));
                    }
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    println!("question-smoke {instance} {thread}: timeout");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(350)).await;
            }
        }
    }
}
