//! Store measurements (ignored by default; they print numbers, not pass/fail):
//!
//! ```sh
//! cargo test --release -p domovoi-server perf_ -- --ignored --nocapture --test-threads=1
//! ```
//!
//! On a file-backed database (WAL, as the server runs), for threads of 100,
//! 1000 and 5000 entities: what one streaming flush costs to append, what the
//! sidebar shell costs to derive (the shell stream does it per event, per
//! subscriber), what resuming a thread costs when other threads were busy, and
//! how many bytes one streamed reply leaves in the event log.
//!
//! And through the code the server runs (a `Turn` on a launched thread, the
//! orchestrator's `dispatch`): commits and time per streamed flush, command
//! latency by thread length, appends with several turns or a shell reader
//! at work, a shell snapshot of many threads, and the bytes a reply's
//! deltas allocate before any of them is flushed.
//!
//! `hot_queries_use_an_index` is a test, not a measurement: it runs with the rest.

use crate::adapters::turn::{Shared, Turn, Work, turn_item};
use crate::db::Db;
use crate::orchestration::commands::Orchestrator;
use crate::orchestration::events::Origin;
use crate::orchestration::projection;
use crate::store::Store;
use crate::test_records::{self, with};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use domovoi_model::typed::{Actor, ConversationMessage, CreationSource, EventBody, ExecutionNode, MessageRole, NodeKind, NodeStatus, Run, Timestamp, TurnItemStatus};

/// Counts the bytes this test binary allocates (growth of a reallocation
/// counts as its new bytes), for `perf_bytes_allocated_by_streamed_deltas`.
struct Counting;
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size.saturating_sub(layout.size()) as u64, Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn temp_store(name: &str) -> (Store, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("domovoi-perf-{name}-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::new(Db::open(&dir.join("state.sqlite")).expect("db"));
    (store, dir)
}

fn origin() -> Origin {
    Origin { driver: Some("claudeAgent".into()), provider_instance_id: Some("claudeAgent".into()), command_id: None }
}

fn at() -> Timestamp {
    Timestamp::from(test_records::AT)
}

/// An assistant's message, as a provider turn writes one.
fn message(thread: &str, id: &str, text: &str, streaming: bool) -> EventBody {
    EventBody::MessageUpdated(Box::new(ConversationMessage {
        notification: None,
        created_by: Actor::Agent,
        creation_source: CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        id: id.into(),
        thread_id: thread.into(),
        run_id: Some("run-1".into()),
        node_id: Some(format!("node-{id}").into()),
        role: MessageRole::Assistant,
        text: text.to_owned(),
        context: None,
        attachments: vec![],
        streaming,
        created_at: at(),
        updated_at: at(),
        delegated_completion: None,
    }))
}

/// The turn item that shows the message `id`.
fn turn_item(thread: &str, id: &str, ordinal: u64, text: &str, streaming: bool) -> EventBody {
    let shared = Shared {
        id: format!("item-{id}").into(),
        thread_id: thread.into(),
        run_id: Some("run-1".into()),
        node_id: Some(format!("node-{id}").into()),
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: if streaming { TurnItemStatus::Running } else { TurnItemStatus::Completed },
        title: None,
        started_at: Some(at()),
        completed_at: None,
        updated_at: at(),
    };
    EventBody::TurnItemUpdated(turn_item!(AssistantMessage(AssistantMessageItem), shared, {
        phase: None,
        message_id: id.into(),
        text: text.to_owned(),
        text_omitted: None,
        attachments: None,
        streaming: streaming,
    }))
}

/// A thread with `n` entities: half messages, half turn items, ~1 KB of text each.
fn seed(store: &Store, thread: &str, n: usize) {
    test_records::append(store, thread, "thread.created", with(test_records::thread(thread, "p1"), json!({ "title": "Perf" })), &origin()).expect("thread");
    fill(store, thread, n);
}

/// `n` more entities on a thread that exists.
fn fill(store: &Store, thread: &str, n: usize) {
    let o = origin();
    let text = "lorem ipsum ".repeat(85);
    for i in 0..n / 2 {
        let id = format!("{thread}-m{i}");
        store.append_event(thread, message(thread, &id, &text, false), &o).expect("message");
        store.append_event(thread, turn_item(thread, &id, i as u64, &text, false), &o).expect("item");
    }
}

/// One streaming flush as `Turn::assistant` emits it: message + turn item, each with the full text so far.
fn flush(store: &Store, thread: &str, text: &str, streaming: bool) {
    let o = origin();
    let id = format!("{thread}-live");
    store.append_event(thread, message(thread, &id, text, streaming), &o).expect("message");
    store.append_event(thread, turn_item(thread, &id, 1_000_000, text, streaming), &o).expect("item");
}

fn per_call(total: Duration, n: u32) -> String {
    format!("{:>9.1} µs", total.as_secs_f64() * 1e6 / f64::from(n))
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_append_and_shell_by_thread_size() {
    for n in [100usize, 1000, 5000] {
        let (store, dir) = temp_store("shell");
        seed(&store, "t1", n);
        let reps = 50u32;
        let started = Instant::now();
        for i in 0..reps {
            flush(&store, "t1", &"word ".repeat(100 + i as usize), true);
        }
        let append = started.elapsed() / 2;
        let started = Instant::now();
        for _ in 0..reps {
            let shell = projection::thread_shell(&store, "t1").expect("shell").expect("thread");
            assert_eq!(shell.id.as_str(), "t1");
        }
        let shell = started.elapsed();
        // The shell stream since the thread cache: per event, ten subscribers
        // ask; the first derives the shell from memory, the rest share it.
        let _ = store.thread_shell("t1").expect("shell");
        let started = Instant::now();
        for i in 0..reps {
            flush(&store, "t1", &"word ".repeat(300 + i as usize), true);
            for _ in 0..10 {
                assert!(store.thread_shell("t1").expect("shell").is_some());
            }
        }
        let cached = started.elapsed();
        println!(
            "perf n={n:>5}: append per event {}   thread_shell from SQL per call {}   flush + 10 subscribers' shells (cached) {}",
            per_call(append, reps), per_call(shell, reps), per_call(cached, reps),
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_resume_after_other_threads_were_busy() {
    for noise in [0usize, 10_000, 50_000] {
        let (store, dir) = temp_store("resume");
        seed(&store, "t1", 100);
        let after = store.latest_sequence().unwrap();
        for i in 0..noise / 2 {
            flush(&store, "noise", &format!("noise {i}"), true);
        }
        for i in 0..25 {
            flush(&store, "t1", &format!("mine {i}"), true);
        }
        let through = store.latest_sequence().unwrap();
        let reps = 20u32;
        let started = Instant::now();
        let mut got = 0;
        for _ in 0..reps {
            got = store.thread_events_after("t1", after, through).expect("events").len();
        }
        assert_eq!(got, 50);
        println!("perf other-thread events={noise:>6}: thread_events_after (50 of mine) {}", per_call(started.elapsed(), reps));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_event_log_bytes_per_streamed_reply() {
    // A 20,000-character reply at ~200 characters/s, flushed on `Turn`'s
    // schedule: every 120 ms, every 250 ms past 8 KiB, then the final event.
    let (store, dir) = temp_store("bytes");
    seed(&store, "t1", 10);
    let before: i64 = store.db().with(|c| c.query_row("SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM events", [], |r| r.get(0))).expect("bytes");
    let (total, rate) = (20_000usize, 200.0_f64);
    let mut t = 0.0_f64;
    let mut flushes = 0;
    loop {
        let len = ((t * rate) as usize).min(total);
        let text = "x".repeat(len);
        let done = len == total;
        flush(&store, "t1", &text, !done);
        flushes += 1;
        if done {
            break;
        }
        t += if len > 8 * 1024 { 0.25 } else { 0.12 };
    }
    let after: i64 = store.db().with(|c| c.query_row("SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM events", [], |r| r.get(0))).expect("bytes");
    let page_bytes: i64 = store.db().with(|c| c.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))).expect("pages");
    println!(
        "perf streamed reply of {total} chars: {flushes} flushes, {} events, {:.1} MB of event payloads ({}x the reply), database {:.1} MB",
        flushes * 2,
        (after - before) as f64 / 1e6,
        (after - before) / total as i64,
        page_bytes as f64 / 1e6,
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---- through the server's own code paths ----

/// An orchestrator on `store`, with the project its threads launch in.
fn orchestrator(store: Store) -> (Orchestrator, tokio::sync::mpsc::UnboundedReceiver<crate::orchestration::commands::Effect>) {
    store
        .create_project(domovoi_contracts::shell::ProjectShell {
            id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: std::env::temp_dir().to_string_lossy().into(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
            favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
            created_at: "t".into(), updated_at: "t".into(),
        }, None)
        .expect("project");
    Orchestrator::new(store)
}

/// Launches thread `id` with a first message (so it has a run) and `n` more entities.
async fn launched(o: &Orchestrator, id: &str, n: usize) {
    let mut launch = crate::test_records::launch(&format!("c-{id}"), id, "p", "codex", "hello");
    launch.initial_message.as_mut().expect("a first message").message_id = Some(format!("m-{id}").into());
    o.launch_thread(launch).await.expect("launch");
    fill(o.store(), id, n);
}

/// The turn of the thread's first run, as a provider adapter holds it.
fn turn_of(store: &Store, thread: &str) -> Turn {
    let runs = store.records::<Run>(thread).expect("runs");
    let run = runs.first().expect("the launch made a run");
    Turn::begin(store.clone(), thread, run.id.as_str()).expect("turn")
}

/// Counts the writer's commits from now on.
fn count_commits(store: &Store) -> Arc<AtomicU64> {
    let commits = Arc::new(AtomicU64::new(0));
    let seen = commits.clone();
    store
        .db()
        .with(|c| c.commit_hook(Some(move || { seen.fetch_add(1, Ordering::Relaxed); false })))
        .expect("commit hook");
    commits
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i].as_secs_f64() * 1e6
}

fn spread(mut times: Vec<Duration>) -> String {
    times.sort();
    format!("p50 {:>8.1} µs  p99 {:>8.1} µs  max {:>8.1} µs", percentile(&times, 0.5), percentile(&times, 0.99), percentile(&times, 1.0))
}

/// One flush as `Turn::assistant` writes it: node, message and turn item.
fn flush3(store: &Store, thread: &str, text: &str) {
    let o = origin();
    let id = format!("{thread}-live");
    let node = ExecutionNode {
        id: format!("node-{id}").into(),
        thread_id: thread.into(),
        run_id: Some("run-1".into()),
        parent_node_id: None,
        root_node_id: format!("node-{id}").into(),
        kind: NodeKind::AssistantMessage,
        status: NodeStatus::Running,
        counts_for_run: false,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(at()),
        completed_at: None,
    };
    store.append_event(thread, EventBody::NodeUpdated(Box::new(node)), &o).expect("node");
    store.append_event(thread, message(thread, &id, text, true), &o).expect("message");
    store.append_event(thread, turn_item(thread, &id, 1_000_000, text, true), &o).expect("item");
}

#[tokio::test]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn perf_streamed_flush_through_turn() {
    for n in [100usize, 1000, 5000] {
        let (store, dir) = temp_store("turn-flush");
        let (o, _effects) = orchestrator(store.clone());
        launched(&o, "t1", n).await;
        let _ = store.thread_shell("t1").expect("shell"); // cached, as with a client watching
        let mut turn = turn_of(&store, "t1");
        let mut live = store.subscribe();
        let commits = count_commits(&store);
        let (flushes, mut text, mut spent) = (20u32, String::new(), Duration::ZERO);
        for _ in 0..flushes {
            text.push_str(&"word ".repeat(40));
            // Past the throttle, so every call writes.
            std::thread::sleep(Duration::from_millis(130));
            let started = Instant::now();
            turn.assistant("live", &text, true, None);
            spent += started.elapsed();
        }
        let mut events = 0u32;
        while live.try_recv().is_ok() {
            events += 1;
        }
        println!(
            "perf n={n:>5}: streamed flush through Turn {}   {:.1} events and {:.1} commits per flush",
            per_call(spent, flushes), f64::from(events) / f64::from(flushes), commits.load(Ordering::Relaxed) as f64 / f64::from(flushes),
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn perf_command_latency_by_thread_size() {
    for n in [100usize, 1000, 5000] {
        let (store, dir) = temp_store("command");
        let (o, _effects) = orchestrator(store.clone());
        launched(&o, "t1", n).await;
        let _ = store.thread_shell("t1").expect("shell");
        let reps = 20u32;
        let (mut visits, mut sends) = (vec![], vec![]);
        for i in 0..reps {
            let visit = json!({ "type": "thread.visit", "commandId": format!("v{i}"), "threadId": "t1", "visitedAt": domovoi_contracts::iso_now() });
            // From the JSON a client sends, as before commands were typed: decoding is part of the cost.
            let started = Instant::now();
            crate::rpc::shell::dispatch_command(&o, visit).await.expect("visit");
            visits.push(started.elapsed());
            let send = json!({ "type": "message.dispatch", "commandId": format!("s{i}"), "threadId": "t1", "messageId": format!("q{i}"), "text": "more",
                "attachments": [], "createdBy": "user", "creationSource": "web", "dispatchMode": { "type": "queue_after_active" } });
            let started = Instant::now();
            crate::rpc::shell::dispatch_command(&o, send).await.expect("send");
            sends.push(started.elapsed());
        }
        println!("perf n={n:>5}: thread.visit {}\n              message.dispatch {}", spread(visits), spread(sends));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_appends_with_concurrent_turns() {
    for turns in [1usize, 4] {
        let (store, dir) = temp_store("turns");
        for t in 0..turns {
            seed(&store, &format!("t{t}"), 1000);
        }
        let workers: Vec<_> = (0..turns)
            .map(|t| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let thread = format!("t{t}");
                    (0..150usize)
                        .map(|i| {
                            let text = "word ".repeat(200 + i);
                            let started = Instant::now();
                            flush3(&store, &thread, &text);
                            started.elapsed()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let times: Vec<Duration> = workers.into_iter().flat_map(|w| w.join().expect("worker")).collect();
        println!("perf {turns} turn(s) streaming at once, 1000-entity threads: one flush (3 events) {}", spread(times));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_flush_with_a_shell_subscriber() {
    // A shell subscriber derives the thread's shell for each event it hears
    // (`rpc::shell`). A flush written as one batch reaches the thread cache
    // whole before the subscriber hears of it; written event by event, the
    // subscriber can be reading the thread while the next event is applied.
    for n in [100usize, 1000, 5000] {
        for (label, subscribed, batched) in [("no subscriber      ", false, true), ("subscriber, batched", true, true), ("subscriber, by event", true, false)] {
            let (store, dir) = temp_store("subscriber");
            seed(&store, "t1", n);
            let _ = store.thread_shell("t1").expect("shell");
            let reader = subscribed.then(|| {
                let (store, mut live) = (store.clone(), store.subscribe());
                std::thread::spawn(move || {
                    while let Ok(event) = live.blocking_recv() {
                        if event.event_type == "thread.deleted" {
                            return;
                        }
                        let _ = store.thread_shell("t1");
                    }
                })
            });
            let times: Vec<Duration> = (0..200usize)
                .map(|i| {
                    let text = "word ".repeat(200 + i);
                    let started = Instant::now();
                    if batched {
                        let _ = store.db().batch(|| -> rusqlite::Result<()> {
                            flush3(&store, "t1", &text);
                            Ok(())
                        });
                    } else {
                        flush3(&store, "t1", &text);
                    }
                    let took = started.elapsed();
                    // A streamed reply flushes every 120 ms: the subscriber is idle by the next one.
                    std::thread::sleep(Duration::from_millis(3));
                    took
                })
                .collect();
            test_records::append(&store, "t1", "thread.deleted", with(test_records::thread("t1", "p1"), json!({ "deletedAt": test_records::AT })), &origin()).expect("end");
            if let Some(r) = reader {
                r.join().expect("reader");
            }
            println!("perf n={n:>5} {label}: one flush (3 events) {}", spread(times));
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_shell_snapshot_of_many_threads() {
    let (store, dir) = temp_store("snapshot");
    for t in 0..500 {
        seed(&store, &format!("t{t}"), 40);
    }
    let reps = 5u32;
    let started = Instant::now();
    for _ in 0..reps {
        let snap = crate::rpc::shell::snapshot_of(&store).expect("snapshot");
        assert_eq!(snap.threads.len(), 500);
    }
    println!("perf shell snapshot, 500 threads of 40 entities: {:.1} ms", started.elapsed().as_secs_f64() * 1e3 / f64::from(reps));
    let _ = std::fs::remove_dir_all(dir);
}

/// A project at `root`, for launches there.
fn project_at(store: &Store, id: &str, root: &std::path::Path) {
    store
        .create_project(domovoi_contracts::shell::ProjectShell {
            id: id.into(), title: id.into(), has_custom_title: false, workspace_root: root.to_string_lossy().into(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
            favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
            created_at: "t".into(), updated_at: "t".into(),
        }, None)
        .expect("project");
}

/// Idle launches (`orchestration.launchThread` with no first message, in the
/// project's own folder), as a client creating threads makes them: in a
/// folder that is no repository, and in a git repository (its branch is
/// looked up). The server's home is set, as it is when it runs.
#[tokio::test]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn perf_launch_idle_thread() {
    let (store, dir) = temp_store("launch");
    let _ = crate::managed::HOME.set(dir.join("home"));
    let (plain, repo) = (dir.join("plain"), dir.join("repo"));
    std::fs::create_dir_all(&plain).expect("plain");
    std::fs::create_dir_all(&repo).expect("repo");
    let git = |args: &[&str]| {
        let done = std::process::Command::new("git").args(args).current_dir(&repo).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().expect("git");
        assert!(done.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["-c", "user.name=perf", "-c", "user.email=perf@example.invalid", "commit", "-q", "--allow-empty", "-m", "init"]);
    project_at(&store, "plain", &plain);
    project_at(&store, "repo", &repo);
    let (o, _effects) = Orchestrator::new(store.clone());
    for project in ["plain", "repo"] {
        let mut times = vec![];
        for i in 0..500 {
            let mut launch = crate::test_records::launch(&format!("c-{project}-{i}"), &format!("{project}-{i}"), project, "codex", "");
            launch.initial_message = None;
            let started = Instant::now();
            let launched = o.launch_thread(launch).await.expect("launch");
            times.push(started.elapsed());
            if project == "repo" && i == 0 {
                assert_eq!(launched.projection.thread.branch.as_deref(), Some("main"), "the branch was looked up");
            }
        }
        println!("perf idle launch, {project:>5}: {}", spread(times));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The shell stream's first items for 1,000 threads: the snapshot, the
/// items, and their JSON, as a subscriber gets them; bytes per item.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_shell_first_items_encode() {
    let (store, dir) = temp_store("shell-items");
    project_at(&store, "p1", &dir);
    for t in 0..1000 {
        test_records::append(&store, &format!("t{t}"), "thread.created", with(test_records::thread(&format!("t{t}"), "p1"), json!({ "title": "Perf" })), &origin()).expect("thread");
    }
    let mut times = vec![];
    let mut bytes = vec![];
    for _ in 0..20 {
        let started = Instant::now();
        let items = crate::rpc::shell::initial_items(crate::rpc::shell::snapshot_of(&store).expect("snapshot"));
        bytes = items.iter().map(|i| serde_json::to_string(&serde_json::to_value(i).expect("value")).expect("text").len()).collect();
        times.push(started.elapsed());
    }
    println!("perf shell first items, 1000 threads: {} ; bytes per item {bytes:?}", spread(times));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn perf_bytes_allocated_by_streamed_deltas() {
    // A 20,000-character reply in 4-character deltas, faster than the flush
    // throttle: what the deltas cost before anything is written.
    let (store, dir) = temp_store("deltas");
    let (o, _effects) = orchestrator(store.clone());
    launched(&o, "t1", 10).await;
    let mut turn = turn_of(&store, "t1");
    let mut text = String::new();
    turn.assistant_delta("live", &mut text, "word");
    let before = ALLOCATED.load(Ordering::Relaxed);
    let started = Instant::now();
    for _ in 1..5000 {
        turn.assistant_delta("live", &mut text, "word");
    }
    let (bytes, took) = (ALLOCATED.load(Ordering::Relaxed) - before, started.elapsed());
    println!(
        "perf 20,000-character reply in 5000 deltas: {:.2} MB allocated ({:.0}x the reply), {:.1} ms",
        bytes as f64 / 1e6, bytes as f64 / text.len() as f64, took.as_secs_f64() * 1e3,
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Every statement a thread's ordinary life runs (launch, a streamed and a
/// finished reply, a command, a resume, a shell, a snapshot) is planned by
/// SQLite with an index or a key, apart from the listed full reads.
#[tokio::test]
async fn hot_queries_use_an_index() {
    // In memory: reads share the writer's connection, so one trace sees all.
    let store = Store::new(Db::open_in_memory().expect("db"));
    let seen: Arc<Mutex<std::collections::BTreeSet<String>>> = Arc::default();
    let log = seen.clone();
    store
        .db()
        .with(|c| {
            c.trace_v2(rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT, Some(move |e: rusqlite::trace::TraceEvent<'_>| {
                if let rusqlite::trace::TraceEvent::Stmt(stmt, _) = e {
                    crate::util::locked(&log).insert(stmt.sql().into_owned());
                }
            }))
        })
        .expect("trace");
    let (o, _effects) = orchestrator(store.clone());
    launched(&o, "t1", 20).await;
    launched(&o, "t2", 4).await;
    let after = store.latest_sequence().expect("sequence");
    let mut turn = turn_of(&store, "t1");
    turn.assistant("live", "Hel", true, None);
    turn.assistant("live", "Hello", false, None);
    let send = domovoi_model::typed::command::MessageDispatch::new("s1", "t1", "q1", "more", domovoi_model::typed::Actor::User, domovoi_model::typed::CreationSource::Web);
    o.dispatch(send.into()).await.expect("send");
    let visit = domovoi_model::typed::command::ThreadVisit {
        command_id: "v1".into(),
        thread_id: "t1".into(),
        visited_at: domovoi_contracts::iso_now().into(),
        opened: None,
        search_query: None,
    };
    o.dispatch(visit.into()).await.expect("visit");
    let through = store.latest_sequence().expect("sequence");
    store.thread_events_after("t1", after, through).expect("resume");
    store.events_after(after, through).expect("shell resume");
    store.thread_shell("t1").expect("shell");
    projection::thread_shell(&store, "t2").expect("shell");
    crate::rpc::shell::snapshot_of(&store).expect("snapshot");

    // Read whole on purpose: the sidebar lists every thread and project, the
    // newest sequence is the end of the key, and SQLite's own tables.
    let whole = ["FROM threads WHERE deleted = 0", "FROM projects WHERE deleted = 0", "MAX(sequence)", "sqlite_", "pragma_"];
    let mut statements = crate::util::locked(&seen).clone();
    assert!(statements.len() > 10, "the workload was traced: {statements:?}");
    // The check can fail: a statement no index serves must be reported.
    let unindexed = "SELECT sequence FROM events WHERE occurred_at = ?1";
    statements.insert(unindexed.to_owned());
    let mut scans = vec![];
    for sql in &statements {
        let head = sql.trim_start().to_ascii_uppercase();
        if !["SELECT", "INSERT", "UPDATE", "DELETE"].iter().any(|k| head.starts_with(k)) || whole.iter().any(|w| sql.contains(w)) {
            continue;
        }
        let plan: Vec<String> = store
            .db()
            .with(|c| {
                let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                // The plan does not depend on the values bound.
                let unbound = rusqlite::params_from_iter(std::iter::repeat_n(rusqlite::types::Null, st.parameter_count()));
                st.query_map(unbound, |r| r.get::<_, String>(3))?.collect()
            })
            .unwrap_or_else(|e| panic!("no plan for {sql}: {e}"));
        if let Some(scan) = plan.iter().find(|line| line.starts_with("SCAN") && !line.contains("USING") && !line.contains("CONSTANT")) {
            scans.push(format!("{scan}\n    {}", sql.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    let control = scans.iter().position(|s| s.contains(unindexed)).expect("the unindexed control statement is reported");
    scans.remove(control);
    assert!(scans.is_empty(), "statements that read a whole table:\n{}", scans.join("\n"));
}

/// A streamed update is its node, message and turn item: three events, one commit.
#[tokio::test]
async fn a_streamed_update_is_one_commit() {
    let store = Store::new(Db::open_in_memory().expect("db"));
    let (o, _effects) = orchestrator(store.clone());
    launched(&o, "t1", 4).await;
    let _ = store.thread_shell("t1").expect("shell"); // cached
    let mut turn = turn_of(&store, "t1");
    let mut live = store.subscribe();
    let commits = count_commits(&store);
    turn.assistant("live", "Hello", true, None);
    assert_eq!(commits.load(Ordering::Relaxed), 1, "the reply's three events commit together");
    let first = live.try_recv().expect("the node's event");
    assert_eq!(first.event_type, "node.updated");
    assert_eq!([live.try_recv().expect("message").event_type.as_str(), live.try_recv().expect("item").event_type.as_str()], ["message.updated", "turn-item.updated"]);
    turn.item("tool-1", Work::Command { input: "ls".into(), output: None, exit_code: None }, TurnItemStatus::Running);
    assert_eq!(commits.load(Ordering::Relaxed), 2, "a tool item's two events commit together");
}
