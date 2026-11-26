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
        issue: None,
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

/// One measurement as `tools/perf.py` reads it: a name, a number and its unit.
fn report(name: &str, value: f64, unit: &str) {
    println!("PERF {name} {value:.4} {unit}");
}

/// The time of one of `n` calls, in microseconds.
fn micros(total: Duration, n: u32) -> f64 {
    total.as_secs_f64() * 1e6 / f64::from(n)
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
        report(&format!("store.append_event.{n}"), micros(append, reps), "us");
        report(&format!("store.thread_shell_from_sql.{n}"), micros(shell, reps), "us");
        report(&format!("store.flush_and_10_shells.{n}"), micros(cached, reps), "us");
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
        let took = started.elapsed();
        println!("perf other-thread events={noise:>6}: thread_events_after (50 of mine) {}", per_call(took, reps));
        report(&format!("store.resume_behind.{noise}"), micros(took, reps), "us");
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
    report("store.reply_20k.event_log_bytes", (after - before) as f64, "bytes");
    let _ = std::fs::remove_dir_all(dir);
}

// ---- through the server's own code paths ----

/// An orchestrator on `store`, with the project its threads launch in.
fn orchestrator(store: Store) -> (Orchestrator, tokio::sync::mpsc::UnboundedReceiver<crate::orchestration::commands::Effect>) {
    store
        .create_project(crate::wire::shell::ProjectShell {
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

fn median_micros(times: &[Duration]) -> f64 {
    let mut sorted = times.to_vec();
    sorted.sort();
    percentile(&sorted, 0.5)
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
        report(&format!("turn.flush.{n}"), micros(spent, flushes), "us");
        report(&format!("turn.events_per_flush.{n}"), f64::from(events) / f64::from(flushes), "count");
        report(&format!("turn.commits_per_flush.{n}"), commits.load(Ordering::Relaxed) as f64 / f64::from(flushes), "count");
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
            let visit = json!({ "type": "thread.visit", "commandId": format!("v{i}"), "threadId": "t1", "visitedAt": crate::time_fmt::iso_now() });
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
        report(&format!("command.thread_visit.{n}.p50"), median_micros(&visits), "us");
        report(&format!("command.message_dispatch.{n}.p50"), median_micros(&sends), "us");
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
        report(&format!("store.flush3_with_{turns}_turns.p50"), median_micros(&times), "us");
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
    let took = started.elapsed();
    println!("perf shell snapshot, 500 threads of 40 entities: {:.1} ms", took.as_secs_f64() * 1e3 / f64::from(reps));
    report("shell.snapshot_500_threads", micros(took, reps), "us");
    let _ = std::fs::remove_dir_all(dir);
}

/// A project at `root`, for launches there.
fn project_at(store: &Store, id: &str, root: &std::path::Path) {
    store
        .create_project(crate::wire::shell::ProjectShell {
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
        report(&format!("launch.idle.{project}.p50"), median_micros(&times), "us");
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
    report("shell.first_items_1000_threads.p50", median_micros(&times), "us");
    report("shell.first_items_1000_threads.bytes", bytes.iter().sum::<usize>() as f64, "bytes");
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
    report("turn.deltas_20k.allocated_bytes", bytes as f64, "bytes");
    report("turn.deltas_20k.time", took.as_secs_f64() * 1e6, "us");
    let _ = std::fs::remove_dir_all(dir);
}

/// One item of a thread subscription as the socket carries it: the frame's bytes.
fn thread_frame(item: &impl serde::Serialize) -> String {
    let value = domovoi_rpc::encoded(item).expect("an item is plain data");
    domovoi_rpc::connection::Outgoing::Chunk { request_id: "1".into(), values: vec![value] }.frame()
}

/// Opening a thread: what its snapshot costs to read from the database and
/// to encode, and how many bytes the client is sent.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn perf_thread_snapshot_by_thread_size() {
    for n in [100usize, 1000, 5000] {
        let (store, dir) = temp_store("thread-snapshot");
        seed(&store, "t1", n);
        let reps = 5u32;
        let (mut build, mut encode, mut pack, mut bytes, mut packed) = (Duration::ZERO, Duration::ZERO, Duration::ZERO, 0, 0);
        for _ in 0..reps {
            let started = Instant::now();
            let (_, item) = crate::orchestration::stream::snapshot_item(&store, "t1", false).expect("snapshot");
            build += started.elapsed();
            let started = Instant::now();
            let frame = thread_frame(&item);
            encode += started.elapsed();
            bytes = frame.len();
            let started = Instant::now();
            packed = domovoi_rpc::packed::pack(frame).len();
            pack += started.elapsed();
        }
        println!(
            "perf n={n:>5}: thread snapshot build {}   encode {}   {:.2} MB   pack {}   {:.2} MB",
            per_call(build, reps), per_call(encode, reps), bytes as f64 / 1e6, per_call(pack, reps), packed as f64 / 1e6,
        );
        report(&format!("thread.snapshot.build.{n}"), micros(build, reps), "us");
        report(&format!("thread.snapshot.encode.{n}"), micros(encode, reps), "us");
        report(&format!("thread.snapshot.bytes.{n}"), bytes as f64, "bytes");
        report(&format!("thread.snapshot.pack.{n}"), micros(pack, reps), "us");
        report(&format!("thread.snapshot.packed_bytes.{n}"), packed as f64, "bytes");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A 20,000-character reply streamed through `Turn` in 20-character deltas
/// over about ten seconds (the flush throttle runs on the clock): the bytes a
/// client watching the thread is sent, the events it is sent them in, and
/// what the reply leaves in the database once it is final.
#[tokio::test]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn perf_wire_and_disk_bytes_per_streamed_reply() {
    let (store, dir) = temp_store("reply-bytes");
    let (o, _effects) = orchestrator(store.clone());
    launched(&o, "t1", 10).await;
    let stored = |store: &Store| -> (i64, i64) {
        store
            .db()
            .with(|c| {
                let events: i64 = c.query_row("SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM events", [], |r| r.get(0))?;
                let records: i64 = c.query_row("SELECT COALESCE(SUM(LENGTH(record)), 0) FROM thread_entities", [], |r| r.get(0))?;
                Ok((events, records))
            })
            .expect("bytes")
    };
    let before = stored(&store);
    let mut turn = turn_of(&store, "t1");
    let mut live = store.subscribe();
    let (mut text, mut sent, mut events, mut peak) = (String::new(), 0usize, 0u32, 0i64);
    let mut drain = |sent: &mut usize, events: &mut u32| {
        while let Ok(event) = live.try_recv() {
            // As a client watching the thread is sent it.
            if event.thread_event.as_ref().is_some_and(|sent| !sent.body.for_the_server_only()) {
                *sent += thread_frame(&crate::orchestration::stream::Sent::Live(event)).len();
                *events += 1;
            }
        }
    };
    for _ in 0..1000 {
        turn.assistant_delta("live", &mut text, "twenty characters...");
        drain(&mut sent, &mut events);
        peak = peak.max(stored(&store).0);
        std::thread::sleep(Duration::from_millis(10));
    }
    turn.assistant("live", &text, false, None);
    drain(&mut sent, &mut events);
    let after = stored(&store);
    let reply = text.len();
    println!(
        "perf streamed reply of {reply} chars through Turn: {events} events, {:.2} MB sent to one subscriber ({:.0}x the reply); event log at its largest {:.2} MB, left {:.2} MB; records left {:.2} MB",
        sent as f64 / 1e6, sent as f64 / reply as f64, (peak - before.0) as f64 / 1e6, (after.0 - before.0) as f64 / 1e6, (after.1 - before.1) as f64 / 1e6,
    );
    report("turn.reply_20k.wire_bytes", sent as f64, "bytes");
    report("turn.reply_20k.events", f64::from(events), "count");
    report("turn.reply_20k.event_log_peak_bytes", (peak - before.0) as f64, "bytes");
    report("turn.reply_20k.event_log_left_bytes", (after.0 - before.0) as f64, "bytes");
    report("turn.reply_20k.records_left_bytes", (after.1 - before.1) as f64, "bytes");
    let _ = std::fs::remove_dir_all(dir);
}

/// On a copy of a real home's database (`DOMOVOI_PERF_HOME` names the home;
/// its database is copied first and the copy is what is opened, so the home
/// is never written): opening it, the shell snapshot, the largest thread's
/// snapshot, and reading every thread as a search does. No server is started
/// on it: one would run the home's scheduled tasks and unfinished work.
#[test]
#[ignore = "measurement over a real home; needs DOMOVOI_PERF_HOME"]
fn perf_real_home() {
    let Ok(home) = std::env::var("DOMOVOI_PERF_HOME") else {
        println!("perf real home: skipped, DOMOVOI_PERF_HOME is not set");
        return;
    };
    let source = std::path::Path::new(&home).join("userdata").join("domovoi.sqlite");
    let dir = std::env::temp_dir().join(format!("domovoi-perf-real-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("domovoi.sqlite");
    std::fs::copy(&source, &path).unwrap_or_else(|e| panic!("{} could not be copied: {e}", source.display()));
    // The first open brings the schema up to date, and the records to this
    // build's format as a server's start does: timed, step by step.
    let size_before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    {
        let db = Db::open(&path).expect("db");
        let started = Instant::now();
        let upgraded = crate::data_format::upgrade(&db, &dir.join("backups")).expect("the records are brought to this format");
        let took = started.elapsed();
        match upgraded {
            Some(done) => {
                println!("perf real home: {done} (in all {:.2} s)", took.as_secs_f64());
                report("migration.backup", done.backup_took.as_secs_f64() * 1e6, "us");
                for (to, _, step) in &done.steps {
                    report(&format!("migration.to_format_{to}"), step.as_secs_f64() * 1e6, "us");
                }
                report("migration.total", took.as_secs_f64() * 1e6, "us");
            }
            None => println!("perf real home: the records are in this build's format already"),
        }
    }
    report("real.database_bytes_before_upgrade", size_before as f64, "bytes");
    let started = Instant::now();
    let store = Store::new(Db::open(&path).expect("db"));
    let opened = started.elapsed();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    // First, before anything has read a thread: opening the largest one as
    // the first thing after a start, then again.
    let largest: String = store
        .db()
        .read(|c| c.query_row("SELECT thread_id FROM thread_entities GROUP BY thread_id ORDER BY SUM(LENGTH(record)) DESC LIMIT 1", [], |r| r.get(0)))
        .expect("a thread with records");
    let started = Instant::now();
    let (_, item) = crate::orchestration::stream::snapshot_item(&store, &largest, false).expect("thread snapshot");
    let build = started.elapsed();
    let started = Instant::now();
    let thread_frame_text = thread_frame(&item);
    let encode = started.elapsed();
    let thread_bytes = thread_frame_text.len();
    let started = Instant::now();
    let thread_packed = domovoi_rpc::packed::pack(thread_frame_text);
    let pack = started.elapsed();
    let started = Instant::now();
    assert_eq!(domovoi_rpc::packed::unpack(&thread_packed).expect("unpacks").len(), thread_bytes);
    let unpack = started.elapsed();
    drop(item);
    let started = Instant::now();
    let again = crate::orchestration::stream::snapshot_item(&store, &largest, false).expect("thread snapshot");
    let build_again = started.elapsed();
    drop(again);

    let started = Instant::now();
    let snapshot = crate::rpc::shell::snapshot_of(&store).expect("shell snapshot");
    let shell_cold = started.elapsed();
    let threads = snapshot.threads.len() + snapshot.archived_threads.len();
    let started = Instant::now();
    let items = crate::rpc::shell::initial_items(crate::rpc::shell::snapshot_of(&store).expect("shell snapshot"));
    let shell_frames: Vec<String> = items.iter().map(|i| thread_frame(i)).collect();
    let shell_bytes: usize = shell_frames.iter().map(String::len).sum();
    let shell_warm = started.elapsed();
    let shell_packed: usize = shell_frames.into_iter().map(|frame| domovoi_rpc::packed::pack(frame).len()).sum();

    let ids: Vec<String> = store.thread_ids().expect("thread ids").into_iter().map(|(id, _)| id).collect();
    let started = Instant::now();
    let mut loaded = 0;
    for id in &ids {
        loaded += usize::from(store.load_thread(id).expect("thread").is_some());
    }
    let every = started.elapsed();
    // Every thread opened once: what the clients are sent, and what packing saves of it.
    let (mut every_bytes, mut every_packed, mut pack_every) = (0usize, 0usize, Duration::ZERO);
    for id in &ids {
        let Ok((_, item)) = crate::orchestration::stream::snapshot_item(&store, id, false) else { continue };
        let frame = thread_frame(&item);
        every_bytes += frame.len();
        let started = Instant::now();
        every_packed += domovoi_rpc::packed::pack(frame).len();
        pack_every += started.elapsed();
    }
    report("real.every_thread.snapshot.bytes", every_bytes as f64, "bytes");
    report("real.every_thread.snapshot.packed_bytes", every_packed as f64, "bytes");
    report("real.every_thread.snapshot.pack", pack_every.as_secs_f64() * 1e6, "us");

    println!(
        "perf real home ({:.0} MB, {threads} threads): open {:.1} ms; shell snapshot cold {:.1} ms, then with its frames {:.1} ms and {:.2} MB; largest thread snapshot build {:.1} ms (again {:.1} ms), encode {:.1} ms, {:.2} MB; reading all {loaded} threads {:.0} ms",
        size as f64 / 1e6, opened.as_secs_f64() * 1e3, shell_cold.as_secs_f64() * 1e3, shell_warm.as_secs_f64() * 1e3, shell_bytes as f64 / 1e6,
        build.as_secs_f64() * 1e3, build_again.as_secs_f64() * 1e3, encode.as_secs_f64() * 1e3, thread_bytes as f64 / 1e6, every.as_secs_f64() * 1e3,
    );
    report("real.largest_thread.snapshot.build_again", build_again.as_secs_f64() * 1e6, "us");
    report("real.database_bytes", size as f64, "bytes");
    report("real.open", opened.as_secs_f64() * 1e6, "us");
    report("real.shell_snapshot.cold", shell_cold.as_secs_f64() * 1e6, "us");
    report("real.shell_snapshot.with_frames", shell_warm.as_secs_f64() * 1e6, "us");
    report("real.shell_snapshot.bytes", shell_bytes as f64, "bytes");
    report("real.largest_thread.snapshot.build", build.as_secs_f64() * 1e6, "us");
    report("real.largest_thread.snapshot.encode", encode.as_secs_f64() * 1e6, "us");
    report("real.largest_thread.snapshot.bytes", thread_bytes as f64, "bytes");
    report("real.largest_thread.snapshot.packed_bytes", thread_packed.len() as f64, "bytes");
    report("real.largest_thread.snapshot.pack", pack.as_secs_f64() * 1e6, "us");
    report("real.largest_thread.snapshot.unpack", unpack.as_secs_f64() * 1e6, "us");
    report("real.shell_snapshot.packed_bytes", shell_packed as f64, "bytes");
    report("real.read_every_thread", every.as_secs_f64() * 1e6, "us");
    drop(store);
    // `DOMOVOI_PERF_KEEP` keeps the upgraded copy, to look at (`doctor --records --home-dir <it>`).
    if std::env::var_os("DOMOVOI_PERF_KEEP").is_some() {
        std::fs::create_dir_all(dir.join("userdata")).expect("userdata");
        std::fs::rename(&path, dir.join("userdata").join("domovoi.sqlite")).expect("the copy is kept");
        println!("perf real home: the upgraded copy is the home {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(dir);
    }
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
        visited_at: crate::time_fmt::iso_now().into(),
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

/// A reply's flush is its turn item, in one commit; the last is the reply's
/// message and the turn item. No node is recorded for an item.
#[tokio::test]
async fn a_streamed_update_is_one_commit() {
    let store = Store::new(Db::open_in_memory().expect("db"));
    let (o, _effects) = orchestrator(store.clone());
    launched(&o, "t1", 4).await;
    let _ = store.thread_shell("t1").expect("shell"); // cached
    let mut turn = turn_of(&store, "t1");
    let mut live = store.subscribe();
    let commits = count_commits(&store);
    let written = |live: &mut tokio::sync::broadcast::Receiver<Arc<crate::store::StoredEvent>>| {
        let mut types = vec![];
        while let Ok(event) = live.try_recv() {
            types.push(event.event_type.clone());
        }
        types
    };
    turn.assistant("live", "Hello", true, None);
    assert_eq!(commits.load(Ordering::Relaxed), 1);
    assert_eq!(written(&mut live), ["turn-item.updated"]);
    std::thread::sleep(Duration::from_millis(130));
    turn.assistant("live", "Hello there", true, None);
    assert_eq!(commits.load(Ordering::Relaxed), 2);
    assert_eq!(written(&mut live), ["turn-item.updated"], "a streaming flush is the turn item alone");
    turn.assistant("live", "Hello there.", false, None);
    assert_eq!(commits.load(Ordering::Relaxed), 3);
    assert_eq!(written(&mut live), ["message.updated", "turn-item.updated"], "the whole reply is written with its message");
    turn.item("tool-1", Work::Command { input: "ls".into(), output: None, exit_code: None }, TurnItemStatus::Running);
    assert_eq!(commits.load(Ordering::Relaxed), 4);
    assert_eq!(written(&mut live), ["turn-item.updated"], "a tool item is one event");
}
