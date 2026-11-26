use super::thread::record_open;
use super::*;
use crate::db::Db;
use crate::orchestration::events::Origin;
use serde_json::json;
use domovoi_contracts::shell::ProjectShell;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::command::{
    MessageDispatch, QueueResume, QueuedRunCancel, QueuedRunEdit, QueuedRunReorder, RunInterrupt, ThreadActiveReorder, ThreadArchive,
    ThreadAutoSettleSet, ThreadDelete, ThreadFork, ThreadMarkUnread, ThreadMergeBack, ThreadMetadataUpdate, ThreadPinReorder, ThreadRuntimeModeSet,
    ThreadSnooze, ThreadTagSet, ThreadTitleRegenerationComplete, ThreadUnsnooze, ThreadVisit, TitleRegenerationOutcome,
};
use domovoi_model::typed::{
    Actor, ContextTransferResolution, ContextTransferStatus, ConversationMessage, CreationSource, DispatchMode, EventBody, ForkSource, ForkSourcePoint,
    HandoffStrategy, InitialMessage, InputIntent, InteractionMode, MessageRole, ModelSelection, NativeRefStrength, ProviderRef, ProviderThread,
    ProviderTurn, ProviderTurnStatus, RunAttempt, RunStatus, RuntimeMode, Thread, ThreadLaunchInput, TitleRegenerationStatus, TurnItem,
    WorkspaceStrategy,
};

fn setup() -> (Orchestrator, mpsc::UnboundedReceiver<Effect>) {
    let store = Store::new(Db::open_in_memory().unwrap());
    store
        .create_project(
            ProjectShell {
                id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: std::env::temp_dir().to_string_lossy().into(),
                repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
                favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
                created_at: "t".into(), updated_at: "t".into(),
            },
            None,
        )
        .unwrap();
    Orchestrator::new(store)
}

fn model(instance: &str, model: &str) -> ModelSelection {
    ModelSelection { instance_id: instance.into(), model: model.into(), options: None }
}

fn launch(id: &str) -> ThreadLaunchInput {
    ThreadLaunchInput {
        command_id: format!("c-{id}").into(),
        creation_source: Some(CreationSource::Web),
        thread_id: Some(id.into()),
        reuse_existing_thread: None,
        project_id: "p".into(),
        title: "Do it".into(),
        generate_title: Some(false),
        model_selection: model("codex", "gpt-6-luna"),
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Default,
        workspace_strategy: WorkspaceStrategy::Root { branch: None },
        initial_message: Some(InitialMessage { message_id: Some("m1".into()), text: "hello".into(), context: None, attachments: vec![] }),
        created_by: None,
        delegated_from: None,
    }
}

/// A message from the user to thread `thread`, with id `id`.
fn send(command: &str, thread: &str, id: &str, text: &str, mode: DispatchMode) -> MessageDispatch {
    let mut message = MessageDispatch::new(command, thread, id, text, Actor::User, CreationSource::Web);
    message.dispatch_mode = mode;
    message
}

/// A message to `t1`.
fn message(id: &str, text: &str, mode: DispatchMode) -> Command {
    send(&format!("c-{id}"), "t1", id, text, mode).into()
}

fn visit(thread: &str, command: &str) -> Command {
    ThreadVisit { command_id: command.into(), thread_id: thread.into(), visited_at: domovoi_contracts::iso_now().into(), opened: None, search_query: None }.into()
}

fn steer(run: &str) -> DispatchMode {
    DispatchMode::SteerActive { target_run_id: run.into() }
}

/// The thread as the store holds it.
fn held(o: &Orchestrator, id: &str) -> Thread {
    o.store.load_thread(id).unwrap().unwrap()
}

#[tokio::test]
async fn a_command_waits_only_for_its_own_thread() {
    let (o, _rx) = setup();
    o.launch_thread(launch("tA")).await.unwrap();
    o.launch_thread(launch("tB")).await.unwrap();
    // tA is busy (say, provisioning a worktree).
    let busy = o.threads.lock("tA").await;
    let other = tokio::time::timeout(std::time::Duration::from_secs(2), o.dispatch(visit("tB", "v1"))).await;
    assert!(matches!(other, Ok(Ok(_))), "another thread's command goes ahead");
    let same = tokio::time::timeout(std::time::Duration::from_millis(200), o.dispatch(visit("tA", "v2"))).await;
    assert!(same.is_err(), "a command on the busy thread waits");
    drop(busy);
    assert!(o.dispatch(visit("tA", "v3")).await.is_ok());
    assert!(crate::util::locked(&o.threads.0).len() <= 1, "unused locks are dropped");
}

#[tokio::test]
async fn a_command_that_fails_part_way_leaves_nothing_and_its_retry_applies_once() {
    let (o, mut rx) = setup();
    let store = o.store().clone();
    o.launch_thread(launch("t1")).await.unwrap();
    let _ = store.thread_shell("t1").unwrap(); // the thread is cached
    while rx.try_recv().is_ok() {}
    let count = |sql: &str| store.db().read(|c| c.query_row(sql, [], |r| r.get::<_, i64>(0))).unwrap();
    let events = count("SELECT COUNT(*) FROM events");
    let runs = held(&o, "t1").runs.len();
    let more = || message("m2", "more", DispatchMode::QueueAfterActive);
    // The receipt, written last, fails: the message, its run and the rest were already written.
    store.db().with(|c| c.execute_batch("CREATE TRIGGER receipt_fails BEFORE INSERT ON command_receipts BEGIN SELECT RAISE(ABORT, 'disk full'); END;")).unwrap();
    let failed = o.dispatch(more()).await.unwrap_err();
    assert!(matches!(failed.refusal, Refusal::Store(_)), "{failed}");
    assert_eq!(count("SELECT COUNT(*) FROM events"), events, "no event of the failed command is kept");
    assert_eq!(held(&o, "t1").runs.len(), runs);
    assert!(rx.try_recv().is_err(), "no effect of the failed command runs");
    let cached = store.thread_shell("t1").unwrap().map(|s| (*s).clone());
    assert_eq!(cached, crate::orchestration::projection::thread_shell(&store, "t1").unwrap(), "the cache saw none of it");
    store.db().with(|c| c.execute_batch("DROP TRIGGER receipt_fails")).unwrap();
    let first = o.dispatch(more()).await.unwrap();
    let again = o.dispatch(more()).await.unwrap(); // the same command again: answered from its receipt
    assert_eq!(first, again);
    assert_eq!(held(&o, "t1").runs.len(), runs + 1, "the retry applies once");
}

#[test]
fn concurrent_edits_of_one_record_lose_nothing() {
    let (o, _rx) = setup();
    let store = o.store().clone();
    // Two counters, each kept in a field of the thread record: its title and its branch.
    let mut record: domovoi_model::typed::AppThread = serde_json::from_value(crate::test_records::thread("t1", "p")).unwrap();
    (record.title, record.branch) = ("0".into(), Some("0".into()));
    store.append_event("t1", EventBody::Thread { change: domovoi_model::typed::ThreadChange::Created, thread: Box::new(record) }, &Origin::default()).unwrap();
    let next = |count: &str| (count.parse::<u64>().unwrap() + 1).to_string();
    let bump = |title: bool| {
        let store = store.clone();
        std::thread::spawn(move || {
            for _ in 0..200 {
                store
                    .update_thread("t1", domovoi_model::typed::ThreadChange::MetadataUpdated, &Origin::default(), |thread| {
                        if title {
                            thread.title = next(&thread.title);
                        } else {
                            thread.branch = thread.branch.as_deref().map(next);
                        }
                        true
                    })
                    .unwrap();
            }
        })
    };
    let (a, b) = (bump(true), bump(false));
    a.join().unwrap();
    b.join().unwrap();
    let thread = store.app_thread("t1").unwrap().unwrap();
    assert_eq!((thread.title.as_str(), thread.branch.as_deref()), ("200", Some("200")), "every edit of either field is kept");
}

#[tokio::test]
async fn the_cached_shell_matches_the_projection_after_every_event() {
    let (o, _rx) = setup();
    let store = o.store().clone();
    o.launch_thread(launch("t1")).await.unwrap();
    let same = |when: &str| {
        let cached = store.thread_shell("t1").unwrap().map(|s| (*s).clone());
        let derived = crate::orchestration::projection::thread_shell(&store, "t1").unwrap();
        assert_eq!(cached, derived, "after {when}");
    };
    same("the launch"); // the thread enters the cache here
    let later = iso_from_millis(now_millis() + 3_600_000);
    let commands: [Command; 5] = [
        visit("t1", "c0"),
        ThreadSnooze { command_id: "c1".into(), thread_id: "t1".into(), snoozed_until: later.into() }.into(),
        ThreadUnsnooze { command_id: "c2".into(), thread_id: "t1".into(), reason: None }.into(),
        send("c3", "t1", "m2", "more", DispatchMode::QueueAfterActive).into(),
        ThreadArchive { command_id: "c4".into(), thread_id: "t1".into() }.into(),
    ];
    for command in commands {
        let what = command.type_name().to_owned();
        o.dispatch(command).await.unwrap();
        same(&what);
    }
    // Writers outside commands: a failed run, a provider thread.
    let thread = held(&o, "t1");
    crate::orchestration::effects::fail_run(&store, "t1", thread.runs.first().unwrap().id.as_str(), "boom", domovoi_model::typed::ProviderFailureClass::ProviderError);
    same("a failed run");
    let provider_thread = thread.provider_threads.first().unwrap().clone();
    store.append_event("t1", EventBody::ProviderThreadUpdated(Box::new(provider_thread)), &Origin::default()).unwrap();
    same("a provider thread update");
    assert!(store.thread_cache().contains("t1"));
}

#[tokio::test]
async fn snoozed_threads_wake_and_can_be_marked_unread() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    let thread = |o: &Orchestrator| o.store.app_thread("t1").unwrap().unwrap();
    let snooze = |command: &str, until: &str| -> Command { ThreadSnooze { command_id: command.into(), thread_id: "t1".into(), snoozed_until: until.into() }.into() };
    let bad = o.dispatch(snooze("s0", "later")).await.unwrap_err();
    assert!(matches!(bad.refusal, Refusal::SnoozeNotATime), "the wake time must be a time");
    let later = iso_from_millis(now_millis() + 3_600_000);
    o.dispatch(snooze("s1", &later)).await.unwrap();
    assert_eq!(thread(&o).snoozed_until.flatten().unwrap().as_str(), later);
    assert!(thread(&o).snoozed_at.flatten().is_some());
    assert_eq!(o.wake_snoozed(), 0, "not due yet");

    o.dispatch(ThreadUnsnooze { command_id: "s2".into(), thread_id: "t1".into(), reason: None }.into()).await.unwrap();
    assert_eq!(thread(&o).snoozed_until, Some(None));
    assert!(thread(&o).last_snooze_wake_at.flatten().is_some());

    let past = iso_from_millis(now_millis() - 1000);
    o.dispatch(snooze("s3", &past)).await.unwrap();
    assert_eq!(o.wake_snoozed(), 1, "a due snooze wakes");
    assert_eq!(thread(&o).snoozed_until, Some(None));
    assert_eq!(thread(&o).last_snooze_wake_at.flatten().unwrap().as_str(), past);
    assert_eq!(o.wake_snoozed(), 0);

    o.dispatch(visit("t1", "v1")).await.unwrap();
    o.dispatch(ThreadMarkUnread { command_id: "u1".into(), thread_id: "t1".into() }.into()).await.unwrap();
    let t = thread(&o);
    assert!(t.last_visited_at.flatten().unwrap() < t.updated_at, "visited before its last activity");
}

/// Marks the thread's first run as running a provider turn. Returns the run's id.
fn run_turn(o: &Orchestrator, tid: &str) -> String {
    let og = Origin::default();
    let mut run = (**held(o, tid).runs.as_slice().first().unwrap()).clone();
    run.status = RunStatus::Running;
    o.store.append_event(tid, EventBody::RunUpdated(Box::new(run.clone())), &og).unwrap();
    let turn = ProviderTurn {
        id: "pturn-1".into(),
        provider_thread_id: run.provider_thread_id.clone().unwrap(),
        node_id: run.root_node_id.clone().unwrap(),
        run_attempt_id: run.active_attempt_id.clone(),
        native_turn_ref: None,
        ordinal: 1,
        status: ProviderTurnStatus::Running,
        started_at: Some("t".into()),
        completed_at: None,
        token_usage: None,
        turn_token_usage: None,
        model: None,
    };
    o.store.append_event(tid, EventBody::ProviderTurnUpdated(Box::new(turn)), &og).unwrap();
    run.id.as_str().to_owned()
}

#[tokio::test]
async fn a_steer_joins_the_running_turn_or_queues_when_it_cannot() {
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    // Not running a provider turn yet: the steer queues.
    o.dispatch(message("m2", "early", steer("run:thread:t1:ordinal:1"))).await.unwrap();
    assert_eq!(held(&o, "t1").runs.as_slice()[1].status, RunStatus::Queued);
    o.dispatch(QueuedRunCancel { command_id: "x".into(), thread_id: "t1".into(), run_id: "run:thread:t1:ordinal:2".into() }.into()).await.unwrap();

    let run = run_turn(&o, "t1");
    o.dispatch(message("m3", "also do this", steer(&run))).await.unwrap();
    let thread = held(&o, "t1");
    assert_eq!(thread.runs.len(), 2, "no new run");
    assert_eq!(thread.messages.get("m3").unwrap().run_id.as_ref().unwrap().as_str(), run);
    let Some(TurnItem::UserMessage(item)) = thread.turn_item_of_message(&"m3".into()) else { panic!("the steer has a turn item") };
    assert_eq!(item.input_intent, InputIntent::Steer);
    assert_eq!(item.provider_turn_id.as_ref().unwrap().as_str(), "pturn-1");
    assert!(item.ordinal > 1_000_001, "after the run's first item");
    assert!(matches!(fx.recv().await, Some(Effect::Steer { message_id, .. }) if message_id == "m3"));

    // The provider refused it: the message becomes a queued run of its own.
    o.requeue_steer("t1", "m3").await;
    let thread = held(&o, "t1");
    let queued = thread.runs.iter().find(|r| r.user_message_id.as_str() == "m3").unwrap();
    assert_eq!(queued.status, RunStatus::Queued);
}

#[tokio::test]
async fn the_queue_can_be_edited_reordered_held_and_restarted_into() {
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    for (id, text) in [("m2", "two"), ("m3", "three"), ("m4", "four")] {
        o.dispatch(message(id, text, DispatchMode::QueueAfterActive)).await.unwrap();
    }
    let order = |o: &Orchestrator| {
        let thread = held(o, "t1");
        let mut queued: Vec<(u64, String)> = thread
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::Queued)
            .map(|r| (r.queue_position.flatten().unwrap(), r.user_message_id.as_str().to_owned()))
            .collect();
        queued.sort();
        queued.into_iter().map(|(_, m)| m).collect::<Vec<_>>()
    };
    let run_of = |o: &Orchestrator, m: &str| held(o, "t1").runs.iter().find(|r| r.user_message_id.as_str() == m).unwrap().id.clone();
    let reorder = |o: &Orchestrator, command: &str, run: &str, before: Option<&str>| -> Command {
        QueuedRunReorder { command_id: command.into(), thread_id: "t1".into(), run_id: run_of(o, run), before_run_id: before.map(|m| run_of(o, m)) }.into()
    };
    let cancel = |o: &Orchestrator, command: &str, run: &str| -> Command {
        QueuedRunCancel { command_id: command.into(), thread_id: "t1".into(), run_id: run_of(o, run) }.into()
    };
    assert_eq!(order(&o), ["m2", "m3", "m4"]);
    o.dispatch(reorder(&o, "r1", "m4", Some("m2"))).await.unwrap();
    assert_eq!(order(&o), ["m4", "m2", "m3"]);
    o.dispatch(reorder(&o, "r2", "m4", None)).await.unwrap();
    assert_eq!(order(&o), ["m2", "m3", "m4"]);
    let edit = QueuedRunEdit { context: None, command_id: "e1".into(), thread_id: "t1".into(), run_id: run_of(&o, "m3"), text: "THREE".into(), attachments: None };
    o.dispatch(edit.into()).await.unwrap();
    assert_eq!(held(&o, "t1").messages.get("m3").unwrap().text, "THREE");
    o.dispatch(cancel(&o, "x1", "m2")).await.unwrap();
    assert_eq!(order(&o), ["m3", "m4"]);
    assert_eq!(held(&o, "t1").runs.iter().find(|r| r.user_message_id.as_str() == "m2").unwrap().status, RunStatus::Cancelled);
    let again = o.dispatch(cancel(&o, "x2", "m2")).await.unwrap_err();
    assert!(matches!(again.refusal, Refusal::RunNotQueued(_)), "only a queued run can be cancelled");

    // Stop and hold: nothing starts until the queue is resumed.
    let stop = RunInterrupt { command_id: "i1".into(), thread_id: "t1".into(), run_id: Some("run:thread:t1:ordinal:1".into()), reason: None, hold_queue: Some(true) };
    o.dispatch(stop.into()).await.unwrap();
    assert!(matches!(fx.recv().await, Some(Effect::Interrupt { .. })));
    let thread = held(&o, "t1");
    assert!(thread.runs.iter().filter(|r| r.status == RunStatus::Queued).all(|r| r.queue_held == Some(true)));
    let mut first = (**thread.runs.as_slice().first().unwrap()).clone();
    first.status = RunStatus::Interrupted;
    o.store.append_event("t1", EventBody::RunUpdated(Box::new(first)), &Origin::default()).unwrap();
    assert_eq!(o.start_next_queued("t1").await.unwrap(), None, "a held queue waits");
    o.dispatch(QueueResume { command_id: "q1".into(), thread_id: "t1".into() }.into()).await.unwrap();
    assert!(matches!(fx.recv().await, Some(Effect::StartTurn { run_id, .. }) if run_id == run_of(&o, "m3").as_str()));

    // Restart: the new message goes first and the active run is stopped.
    let active = run_of(&o, "m3");
    o.dispatch(message("m5", "instead", DispatchMode::RestartActive { target_run_id: active.clone() })).await.unwrap();
    assert!(matches!(fx.recv().await, Some(Effect::Interrupt { run_id, .. }) if run_id == active.as_str()));
    assert_eq!(order(&o), ["m5", "m4"]);
}

#[tokio::test]
async fn deleting_a_thread_stops_its_work_and_sidebar_fields_are_kept() {
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    let thread = |o: &Orchestrator| o.store.app_thread("t1").unwrap().unwrap();
    let tag = |command: &str, tag: &str, tagged: bool| -> Command {
        ThreadTagSet { command_id: command.into(), thread_id: "t1".into(), tag: tag.into(), tagged }.into()
    };
    o.dispatch(tag("g1", "bug", true)).await.unwrap();
    o.dispatch(tag("g2", "ui", true)).await.unwrap();
    o.dispatch(tag("g3", "bug", false)).await.unwrap();
    assert_eq!(thread(&o).tags.unwrap(), ["ui"]);
    assert!(matches!(o.dispatch(tag("g4", "a\nb", true)).await.unwrap_err().refusal, Refusal::InvalidTag));
    o.dispatch(ThreadPinReorder { command_id: "p1".into(), thread_id: "t1".into(), order_key: "a0".into() }.into()).await.unwrap();
    o.dispatch(ThreadActiveReorder { command_id: "p2".into(), thread_id: "t1".into(), order_key: "b0".into() }.into()).await.unwrap();
    let auto_settle = |command: &str, enabled: bool| -> Command { ThreadAutoSettleSet { command_id: command.into(), thread_id: "t1".into(), enabled }.into() };
    o.dispatch(auto_settle("a1", false)).await.unwrap();
    let t = thread(&o);
    assert_eq!((t.pin_order_key.flatten().as_deref(), t.active_order_key.flatten().as_deref()), (Some("a0"), Some("b0")));
    assert!(t.auto_settle_disabled_at.flatten().is_some());
    o.dispatch(auto_settle("a2", true)).await.unwrap();
    assert_eq!(thread(&o).auto_settle_disabled_at, Some(None));

    o.dispatch(message("m2", "later", DispatchMode::QueueAfterActive)).await.unwrap();
    o.dispatch(ThreadDelete { command_id: "d1".into(), thread_id: "t1".into(), only_if_unused: None }.into()).await.unwrap();
    assert!(matches!(fx.recv().await, Some(Effect::Interrupt { .. })));
    assert!(matches!(fx.recv().await, Some(Effect::CloseTerminals { .. })));
    assert_eq!(held(&o, "t1").runs.as_slice()[1].status, RunStatus::Cancelled);
    assert!(o.store.app_thread("t1").unwrap().unwrap().deleted_at.is_some());
    let gone = o.dispatch(visit("t1", "v9")).await.unwrap_err();
    assert!(matches!(gone.refusal, Refusal::ThreadDeleted));
}

#[tokio::test]
async fn a_generated_title_replaces_the_seed_only_for_its_own_request() {
    let (o, mut fx) = setup();
    let mut l = launch("t1");
    l.generate_title = Some(true);
    o.launch_thread(l).await.unwrap();
    let Some(Effect::GenerateTitle { request_id, regenerate: false, .. }) = fx.recv().await else { panic!("a title request") };
    let thread = |o: &Orchestrator| o.store.app_thread("t1").unwrap().unwrap();
    assert_eq!(thread(&o).title_regeneration.flatten().unwrap().request_id.as_str(), request_id);
    let complete = |command: &str, request: &str, title: &str, status: Option<TitleRegenerationStatus>| -> Command {
        ThreadTitleRegenerationComplete {
            command_id: command.into(),
            thread_id: "t1".into(),
            request_id: request.into(),
            title: Some(title.into()),
            outcome: status.map(|status| TitleRegenerationOutcome { status, message: None }),
        }
        .into()
    };
    // A stale answer (another request) changes nothing.
    o.dispatch(complete("x1", "old", "Stale", None)).await.unwrap();
    assert_eq!(thread(&o).title, "Do it");
    o.dispatch(complete("x2", &request_id, "Search Bar", Some(TitleRegenerationStatus::Renamed))).await.unwrap();
    let t = thread(&o);
    assert_eq!(t.title, "Search Bar");
    assert_eq!(t.title_regeneration, Some(None));
    assert_eq!(t.last_title_regeneration.flatten().unwrap().status, TitleRegenerationStatus::Renamed);
    // Asking again regenerates from the whole thread.
    let regenerate = ThreadMetadataUpdate {
        command_id: "r1".into(),
        thread_id: "t1".into(),
        title: None,
        regenerate_title: Some(true),
        branch: None,
        worktree_path: None,
        expected_worktree_path: None,
        expected_branch: None,
        expected_empty: None,
        limit_recovery: None,
        linked_pull_request: None,
    };
    o.dispatch(regenerate.into()).await.unwrap();
    loop {
        match fx.recv().await {
            Some(Effect::GenerateTitle { request_id, regenerate: true, .. }) => break assert_eq!(request_id, "r1"),
            Some(_) => continue,
            None => panic!("no regeneration"),
        }
    }
    assert_eq!(thread(&o).title_regeneration.flatten().unwrap().request_id.as_str(), "r1");
}

/// Finishes a thread's latest run as a provider would: a native thread and
/// turn, the agent's reply, status completed. Returns the run as it then is.
fn finish_run(o: &Orchestrator, tid: &str, reply: &str) -> Run {
    let thread = held(o, tid);
    let mut run = (**thread.runs.as_slice().iter().max_by_key(|r| r.ordinal).unwrap()).clone();
    let og = Origin::default();
    let provider_thread_id = run.provider_thread_id.clone().unwrap();
    let mut provider_thread: ProviderThread = thread.provider_threads.get(provider_thread_id.as_str()).unwrap().clone();
    let strong = |native_id: String| ProviderRef { driver: provider_thread.driver.clone(), native_id: Some(native_id), strength: NativeRefStrength::Strong, fingerprint: None, ordinal: None };
    let (thread_ref, turn_ref) = (strong(format!("native-{tid}")), strong(format!("turn-{}", run.ordinal)));
    provider_thread.native_thread_ref = Some(thread_ref);
    o.store.append_event(tid, EventBody::ProviderThreadUpdated(Box::new(provider_thread)), &og).unwrap();
    let turn = ProviderTurn {
        id: format!("pturn-{}", run.id).into(),
        provider_thread_id,
        node_id: run.root_node_id.clone().unwrap(),
        run_attempt_id: run.active_attempt_id.clone(),
        native_turn_ref: Some(turn_ref),
        ordinal: run.ordinal,
        status: ProviderTurnStatus::Completed,
        started_at: Some("t".into()),
        completed_at: Some("t".into()),
        token_usage: None,
        turn_token_usage: None,
        model: None,
    };
    let attempt = run.active_attempt_id.clone().unwrap();
    o.store.update_record::<RunAttempt>(tid, attempt.as_str(), &og, |attempt| attempt.provider_turn_id = Some(turn.id.clone())).unwrap().unwrap();
    o.store.append_event(tid, EventBody::ProviderTurnUpdated(Box::new(turn)), &og).unwrap();
    let said = ConversationMessage {
        notification: None,
        created_by: Actor::Agent,
        creation_source: CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        id: format!("reply-{}", run.id).into(),
        thread_id: tid.into(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone(),
        role: MessageRole::Assistant,
        text: reply.into(),
        context: None,
        attachments: vec![],
        streaming: false,
        created_at: "z".into(),
        updated_at: "z".into(),
        delegated_completion: None,
    };
    o.store.append_event(tid, EventBody::MessageUpdated(Box::new(said)), &og).unwrap();
    run.status = RunStatus::Completed;
    o.store.append_event(tid, EventBody::RunUpdated(Box::new(run.clone())), &og).unwrap();
    run
}

/// Message `id` to `thread`, on another model if one is given.
fn say(thread: &str, id: &str, on: Option<ModelSelection>) -> Command {
    let mut message = send(&format!("c-{thread}-{id}"), thread, &format!("{thread}-{id}"), &format!("message {id}"), DispatchMode::QueueAfterActive);
    message.model_selection = on;
    message.into()
}

fn fork(command: &str, source: &str, target: &str) -> Command {
    ThreadFork {
        created_by: Actor::User,
        creation_source: CreationSource::Web,
        command_id: command.into(),
        source_thread_id: source.into(),
        target_thread_id: target.into(),
        source_point: ForkSourcePoint::LatestStable,
        title: None,
        side_chat: None,
        created_at: None,
    }
    .into()
}

#[tokio::test]
async fn a_fork_continues_natively_on_the_same_provider_and_portably_on_another() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    let source_run = finish_run(&o, "t1", "Added the search bar.");
    o.dispatch(fork("f-t2", "t1", "t2")).await.unwrap();
    let t2 = held(&o, "t2");
    assert_eq!(t2.record.title, "Do it fork");
    assert_eq!(json!(t2.record.lineage), json!({ "parentThreadId": "t1", "relationshipToParent": "fork", "rootThreadId": "t1" }));
    assert_eq!(t2.record.forked_from, Some(ForkSource::Run { thread_id: "t1".into(), run_id: source_run.id.clone() }));
    let transfer = t2.context_transfers.first().unwrap();
    assert_eq!(transfer.status, ContextTransferStatus::Pending);
    assert_eq!(transfer.source_point.provider_thread_ref.as_ref().unwrap().native_id.as_deref(), Some("native-t1"));
    let again = o.dispatch(fork("f-again", "t1", "t2")).await.unwrap_err();
    assert!(matches!(again.refusal, Refusal::ThreadExists(_)), "the target must be new");

    // Same provider: the provider thread forks the native conversation.
    o.dispatch(say("t2", "1", None)).await.unwrap();
    let t2 = held(&o, "t2");
    let transfer = t2.context_transfers.first().unwrap();
    assert_eq!(transfer.status, ContextTransferStatus::Consumed);
    assert!(matches!(transfer.resolution, Some(ContextTransferResolution::NativeFork { .. })));
    let run = t2.runs.first().unwrap();
    assert_eq!(transfer.target_run_id.as_ref(), Some(&run.id));
    assert_eq!(t2.provider_threads.first().unwrap().forked_from.as_ref().map(|from| &from.provider_thread_id), source_run.provider_thread_id.as_ref());
    assert!(run.context_handoff_id.is_none());

    // Another provider: the conversation goes over as text.
    o.dispatch(fork("f-t3", "t1", "t3")).await.unwrap();
    o.dispatch(say("t3", "1", Some(model("claudeAgent", "claude-haiku-5-5")))).await.unwrap();
    let t3 = held(&o, "t3");
    let handoff = t3.context_handoffs.first().unwrap();
    assert_eq!(t3.context_transfers.first().unwrap().resolution, Some(ContextTransferResolution::PortableContext { context_handoff_id: handoff.id.clone() }));
    assert_eq!(t3.runs.first().unwrap().context_handoff_id.as_ref(), Some(&handoff.id));
    assert!(handoff.summary_text.contains("User: hello\nAssistant: Added the search bar."), "{}", handoff.summary_text);
    let item = t3.turn_items.iter().find_map(|item| if let TurnItem::Handoff(item) = item { Some(item) } else { None }).expect("a handoff item");
    assert_eq!(item.to_provider_instance_id.as_str(), "claudeAgent");
    assert_eq!(item.from_provider_instance_ids, ["codex".into()]);
}

#[tokio::test]
async fn switching_provider_hands_over_the_thread_and_a_fork_merges_back() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    finish_run(&o, "t1", "Reply one.");
    let switch = domovoi_model::typed::command::ProviderSwitch { command_id: "sw".into(), thread_id: "t1".into(), model_selection: model("claudeAgent", "claude-haiku-5-5") };
    o.dispatch(switch.into()).await.unwrap();
    o.dispatch(say("t1", "2", None)).await.unwrap();
    let t1 = held(&o, "t1");
    let run = t1.runs.iter().find(|r| r.ordinal == 2).unwrap();
    assert_eq!(run.provider_instance_id.as_str(), "claudeAgent");
    let handoff = t1.context_handoffs.iter().find(|h| Some(&h.id) == run.context_handoff_id.as_ref()).expect("a handoff for the switch");
    assert!(handoff.summary_text.contains("Reply one."));
    assert_eq!((handoff.covered_run_ordinals.from, handoff.covered_run_ordinals.to), (1, 1));
    let provider_thread = t1.provider_threads.iter().find(|p| Some(&p.id) == run.provider_thread_id.as_ref()).unwrap();
    assert_eq!(provider_thread.handoff_ids, std::slice::from_ref(&handoff.id));
    finish_run(&o, "t1", "Reply two.");
    // The same provider again: no handoff.
    o.dispatch(say("t1", "3", None)).await.unwrap();
    assert!(held(&o, "t1").runs.iter().find(|r| r.ordinal == 3).unwrap().context_handoff_id.is_none());
    finish_run(&o, "t1", "Reply three.");

    o.dispatch(fork("f", "t1", "f1")).await.unwrap();
    o.dispatch(say("f1", "1", None)).await.unwrap();
    finish_run(&o, "f1", "Tried the other approach; it works.");
    let back = |source: &str, target: &str| -> Command {
        ThreadMergeBack {
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            command_id: format!("mb-{source}").into(),
            source_thread_id: source.into(),
            target_thread_id: target.into(),
            source_point: ForkSourcePoint::LatestStable,
            created_at: None,
        }
        .into()
    };
    let wrong_way = o.dispatch(back("t1", "f1")).await.unwrap_err();
    assert!(matches!(wrong_way.refusal, Refusal::NotAForkOfTarget), "only a fork merges back, into its parent");
    o.dispatch(back("f1", "t1")).await.unwrap();
    o.dispatch(say("t1", "4", None)).await.unwrap();
    let t1 = held(&o, "t1");
    let run = t1.runs.iter().find(|r| r.ordinal == 4).unwrap();
    let handoff = t1.context_handoffs.iter().find(|h| Some(&h.id) == run.context_handoff_id.as_ref()).expect("a merge-back handoff");
    assert_eq!(handoff.strategy, HandoffStrategy::ForkDeltaSummary);
    assert!(handoff.summary_text.contains("it works"));
}

#[tokio::test]
async fn durable_effects_wait_in_the_outbox_until_done() {
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    o.send_effect(Effect::NameBranch { thread_id: "t1".into() });
    o.send_effect(Effect::Interrupt { thread_id: "t1".into(), run_id: "r".into() });
    assert_eq!(o.store.outbox_all::<Effect>().unwrap().len(), 1, "only effects that outlive a process are kept");
    fx.recv().await;
    fx.recv().await;
    // A restart before the worker finished: it comes back.
    assert_eq!(o.replay_outbox(), 1);
    assert!(matches!(fx.recv().await, Some(Effect::NameBranch { thread_id }) if thread_id == "t1"));
    o.store.outbox_remove(&Effect::NameBranch { thread_id: "t1".into() }.outbox_key().unwrap()).unwrap();
    assert_eq!(o.replay_outbox(), 0);
}

#[tokio::test]
async fn launch_emits_thread_run_message_and_starts_turn() {
    let (o, mut fx) = setup();
    let launched = o.launch_thread(launch("t1")).await.unwrap();
    let p = &launched.projection;
    assert_eq!(p.thread.title, "Do it");
    assert_eq!(p.runs[0].id.as_str(), "run:thread:t1:ordinal:1");
    assert_eq!(p.runs[0].status, RunStatus::Starting);
    assert_eq!(p.messages[0].text, "hello");
    assert_eq!(p.turn_items[0].ordinal(), 1_000_001);
    assert!(p.visible_turn_items.is_empty(), "the timeline as shown is derived by the reader");
    assert_eq!(p.thread.active_provider_thread_id.as_ref(), Some(&p.provider_threads[0].id));
    assert!(matches!(fx.recv().await, Some(Effect::StartTurn { .. })));
    // Idempotent by commandId.
    let again = o.launch_thread(launch("t1")).await.unwrap();
    assert_eq!(again, launched);
    // A launch's id is not a command's.
    let reused = o.dispatch(visit("t1", "c-t1")).await;
    assert!(reused.is_ok(), "a launch with a first message answers a command of the same id with that message's sequence");
    let mut other = launch("t2");
    other.command_id = "c-t1".into();
    assert_eq!(o.launch_thread(other).await.unwrap().thread_id.as_str(), "t1", "the launch already answered is answered again");
}

#[tokio::test]
async fn second_message_queues_until_the_active_run_settles() {
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    o.dispatch(message("m2", "next", DispatchMode::QueueAfterActive)).await.unwrap();
    let thread = held(&o, "t1");
    let queued = &thread.runs.as_slice()[1];
    assert_eq!((&queued.status, queued.queue_position), (&RunStatus::Queued, Some(Some(1))));
    assert!(fx.try_recv().is_err(), "queued run must not start");
    // Settle run 1, then the queue advances.
    let mut first = (**thread.runs.as_slice().first().unwrap()).clone();
    first.status = RunStatus::Completed;
    o.store.append_event("t1", EventBody::RunUpdated(Box::new(first)), &Origin::default()).unwrap();
    assert_eq!(o.start_next_queued("t1").await.unwrap().as_deref(), Some("run:thread:t1:ordinal:2"));
    let thread = held(&o, "t1");
    assert_eq!(thread.runs.as_slice()[1].status, RunStatus::Starting);
    assert_eq!(thread.turn_items.len(), 2);
}

#[tokio::test]
async fn visit_and_runtime_mode() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    let opened = ThreadVisit { command_id: "v".into(), thread_id: "t1".into(), visited_at: "2099-01-01T00:00:00.000Z".into(), opened: Some(true), search_query: None };
    o.dispatch(opened.into()).await.unwrap();
    let t = o.store.app_thread("t1").unwrap().unwrap();
    assert_eq!(t.last_visited_at.flatten().unwrap().as_str(), "2099-01-01T00:00:00.000Z");
    assert_eq!(t.opens.flatten().unwrap().score.as_f64(), Some(1.0));
    let mode = ThreadRuntimeModeSet { command_id: "rm".into(), thread_id: "t1".into(), runtime_mode: RuntimeMode::AutoAcceptEdits };
    o.dispatch(mode.into()).await.unwrap();
    assert_eq!(o.store.app_thread("t1").unwrap().unwrap().runtime_mode, RuntimeMode::AutoAcceptEdits);
    // A command t3code defines and Domovoi does not carry out, and one nobody defines.
    let release = domovoi_model::typed::command::PreparedRunRelease { command_id: "f".into(), thread_id: "t1".into(), run_id: "r".into() };
    let refused = o.dispatch(release.into()).await.unwrap_err();
    assert_eq!(
        json!(refused),
        json!({ "_tag": "OrchestrationV2DispatchCommandError", "commandId": "f", "commandType": "prepared-run.release",
            "message": "Command prepared-run.release is not supported by domovoi-server yet." })
    );
    let unknown = Command::from_value(json!({ "type": "thread.teleport", "commandId": "g", "threadId": "t1" })).unwrap();
    assert_eq!(o.dispatch(unknown).await.unwrap_err().to_string(), "Command thread.teleport is not supported by domovoi-server yet.");
}

#[test]
fn frecency_decays_and_keeps_picks() {
    let first = record_open(None, 0, Some("Hello"));
    assert_eq!(first.picks[0].query, "hello");
    let same_window = record_open(Some(&first), 60_000, None);
    assert_eq!(same_window.opened_at.as_i64(), Some(0));
    let later = record_open(Some(&first), 14 * 24 * 3600 * 1000, None);
    assert!((later.score.as_f64().unwrap() - 1.5).abs() < 1e-9);
}

#[tokio::test]
async fn a_fresh_context_message_starts_a_new_session_that_later_messages_continue() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    let first = finish_run(&o, "t1", "Did one.");
    let mut fresh = send("c-fresh", "t1", "m2", "start over", DispatchMode::QueueAfterActive);
    fresh.fresh_context = Some(true);
    o.dispatch(fresh.into()).await.unwrap();
    let t = held(&o, "t1");
    let run2 = t.runs.iter().find(|r| r.ordinal == 2).unwrap();
    assert_eq!(run2.fresh_context, Some(true));
    assert_ne!(run2.provider_thread_id, first.provider_thread_id, "a provider session of its own");
    assert!(t.context_handoffs.is_empty(), "nothing from before is handed over");
    let Some(TurnItem::UserMessage(item)) = t.turn_item_of_message(&"m2".into()) else { panic!("the message's turn item") };
    assert_eq!(item.fresh_context, Some(true));

    // The next message continues the fresh session.
    let second = finish_run(&o, "t1", "Started over.");
    o.dispatch(say("t1", "3", None)).await.unwrap();
    let run3 = held(&o, "t1").runs.iter().find(|r| r.ordinal == 3).unwrap().provider_thread_id.clone();
    assert_eq!(run3, second.provider_thread_id);

    // Another provider gets the conversation since the fresh start only.
    finish_run(&o, "t1", "Third.");
    o.dispatch(say("t1", "4", Some(model("claudeAgent", "claude-haiku-5-5")))).await.unwrap();
    let t = held(&o, "t1");
    let handoff = t.context_handoffs.iter().next().expect("a handoff");
    assert_eq!((handoff.covered_run_ordinals.from, handoff.covered_run_ordinals.to), (2, 3));
}

#[tokio::test]
async fn a_scheduled_fresh_session_is_a_side_conversation() {
    let (o, _fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    let first = finish_run(&o, "t1", "Did one.");
    let mut scheduled = send("c-sched", "t1", "m2", "daily check", DispatchMode::QueueAfterActive);
    scheduled.fresh_context = Some(true);
    scheduled.scheduled_task_id = Some("task-1".into());
    o.dispatch(scheduled.into()).await.unwrap();
    let side = finish_run(&o, "t1", "All good.");
    assert_ne!(side.provider_thread_id, first.provider_thread_id);
    // The user's next message continues the thread's own conversation.
    o.dispatch(say("t1", "3", None)).await.unwrap();
    let run3 = held(&o, "t1").runs.iter().find(|r| r.ordinal == 3).unwrap().provider_thread_id.clone();
    assert_eq!(run3, first.provider_thread_id);
}

#[tokio::test]
async fn issues_link_once_and_unlink_by_tracker_level_key() {
    use domovoi_model::typed::ThreadIssueLinkSource;
    use domovoi_model::typed::command::{ThreadIssueLinkCommand, ThreadIssueUnlink};
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    let link = |command: &str, host: &str, key: &str| -> Command {
        ThreadIssueLinkCommand {
            command_id: command.into(),
            thread_id: "t1".into(),
            tracker: "github".into(),
            host: host.into(),
            scope: "o/r".into(),
            key: key.into(),
            project_id: "p1".into(),
            url: format!("https://github.com/o/r/issues/{key}"),
            display_key: format!("#{key}"),
            title: "A bug".into(),
            source: ThreadIssueLinkSource::Agent,
        }
        .into()
    };
    let keys = |o: &Orchestrator| -> Vec<String> {
        o.store.app_thread("t1").unwrap().unwrap().issues.unwrap_or_default().into_iter().map(|l| format!("{}/{}", l.key.host, l.key.key)).collect()
    };
    o.dispatch(link("i1", " GitHub.com", "7")).await.unwrap();
    let before = o.store.latest_sequence().unwrap();
    o.dispatch(link("i2", "github.com", "7 ")).await.unwrap();
    assert_eq!(o.store.latest_sequence().unwrap(), before, "the same issue twice is one link and no event");
    o.dispatch(link("i3", "github.com", "8")).await.unwrap();
    assert_eq!(keys(&o), ["github.com/7", "github.com/8"], "hosts are stored lower-cased and trimmed");
    let unlink = ThreadIssueUnlink { command_id: "u1".into(), thread_id: "t1".into(), tracker: "github".into(), host: "GITHUB.COM".into(), scope: "O/R".into(), key: "7".into() };
    o.dispatch(unlink.into()).await.unwrap();
    assert_eq!(keys(&o), ["github.com/8"]);
}

#[tokio::test]
async fn pull_requests_link_once_unlink_to_a_tombstone_in_a_stack_and_watch() {
    use domovoi_model::typed::command::{PullRequestToLink, ThreadPullRequestLink, ThreadPullRequestUnlink, ThreadPullRequestWatch};
    use domovoi_model::typed::{PullRequestLinkSource, PullRequestStack, PullRequestStackKind, PullRequestStackLayer, PullRequestState, PullRequestWatchUntil};
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    let link = |command: &str, number: u64, source: PullRequestLinkSource| -> Command {
        Command::ThreadPullRequestLink(Box::new(ThreadPullRequestLink {
            command_id: command.into(),
            thread_id: "t1".into(),
            host: "GitHub.com".into(),
            repository: "Acme/App".into(),
            number,
            url: format!("https://github.com/acme/app/pull/{number}"),
            source,
            repository_web_url: None,
        }))
    };
    let unlink = |command: &str, number: u64| -> Command {
        Command::ThreadPullRequestUnlink(Box::new(ThreadPullRequestUnlink { command_id: command.into(), thread_id: "t1".into(), host: "github.com".into(), repository: "acme/app".into(), number }))
    };
    let watch = |command: &str, number: u64, watching: bool, until: Option<PullRequestWatchUntil>| -> Command {
        Command::ThreadPullRequestWatch(Box::new(ThreadPullRequestWatch {
            command_id: command.into(),
            thread_id: "t1".into(),
            host: "github.com".into(),
            repository: "acme/app".into(),
            number,
            watching,
            until,
            checks_pending: Some(false),
            link: Some(PullRequestToLink { url: format!("https://github.com/acme/app/pull/{number}"), source: PullRequestLinkSource::Agent, repository_web_url: None }),
        }))
    };
    let links = |o: &Orchestrator| o.store.app_thread("t1").unwrap().unwrap().pull_requests.unwrap_or_default();
    o.dispatch(link("l1", 7, PullRequestLinkSource::Agent)).await.unwrap();
    let before = o.store.latest_sequence().unwrap();
    o.dispatch(link("l2", 7, PullRequestLinkSource::Manual)).await.unwrap();
    assert_eq!(o.store.latest_sequence().unwrap(), before, "the same pull request twice is one link and no event");
    assert_eq!(links(&o).len(), 1);
    assert_eq!((links(&o)[0].host.as_str(), links(&o)[0].repository.as_str()), ("github.com", "acme/app"), "keys are lower-cased");
    let event = o.store.events_after(before - 1, before).unwrap().into_iter().last().unwrap();
    assert_eq!(event.event_type, "thread.pull-request-synced");

    // Watching starts a watch that carries `until: merged`; unwatching keeps it as ended.
    o.dispatch(watch("w1", 7, true, Some(PullRequestWatchUntil::Merged))).await.unwrap();
    let w = links(&o)[0].watch.clone().unwrap();
    assert_eq!((w.until, w.checks_pending), (Some(PullRequestWatchUntil::Merged), None));
    o.dispatch(watch("w2", 7, false, None)).await.unwrap();
    assert!(links(&o)[0].watch.is_none() && links(&o)[0].ended_watch.is_some());

    // A watch links a pull request not linked yet.
    o.dispatch(watch("w3", 9, true, None)).await.unwrap();
    assert_eq!(links(&o).iter().map(|l| (l.number, l.watch.is_some())).collect::<Vec<_>>(), [(7, false), (9, true)]);

    // A plain link goes; a stack member stays as a tombstone, and a manual link brings it back.
    o.dispatch(unlink("u1", 9)).await.unwrap();
    assert_eq!(links(&o).iter().map(|l| l.number).collect::<Vec<_>>(), [7]);
    let mut thread = o.store.app_thread("t1").unwrap().unwrap();
    let layer = |number| PullRequestStackLayer { number, head_branch: format!("b{number}"), state: PullRequestState::Open };
    thread.pull_requests.as_mut().unwrap()[0].stack =
        Some(PullRequestStack { kind: PullRequestStackKind::Native, id: "s".into(), number: 7, url: "u".into(), base: "main".into(), layers: vec![layer(6), layer(7)] });
    o.store.append_event("t1", EventBody::Thread { change: domovoi_model::typed::ThreadChange::PullRequestSynced, thread: Box::new(thread) }, &Origin::default()).unwrap();
    o.dispatch(unlink("u2", 7)).await.unwrap();
    assert_eq!(links(&o)[0].source, PullRequestLinkSource::StackDismissed);
    o.dispatch(link("l3", 7, PullRequestLinkSource::Stack)).await.unwrap();
    assert_eq!(links(&o)[0].source, PullRequestLinkSource::StackDismissed, "a stack sync does not bring it back");
    o.dispatch(link("l4", 7, PullRequestLinkSource::Manual)).await.unwrap();
    assert_eq!(links(&o)[0].source, PullRequestLinkSource::Manual);

    // A settled thread watches nothing.
    o.dispatch(ThreadArchive { command_id: "a1".into(), thread_id: "t1".into() }.into()).await.unwrap();
    let refused = o.dispatch(watch("w4", 7, true, None)).await.unwrap_err();
    assert!(refused.to_string().contains("cannot watch pull requests"), "{refused}");
}

#[tokio::test]
async fn a_secret_card_is_asked_under_the_run_root_and_ends_once() {
    use domovoi_model::typed::command::SecretRequestRecord;
    use domovoi_model::typed::{SecretRequestStatus, TurnItemStatus};
    let (o, mut fx) = setup();
    o.launch_thread(launch("t1")).await.unwrap();
    fx.recv().await;
    let run_id = run_turn(&o, "t1");
    let root = held(&o, "t1").runs.as_slice().first().unwrap().root_node_id.clone().unwrap();
    let record = |command: &str, node: &str, status: SecretRequestStatus| -> Command {
        Command::SecretRequestRecord(Box::new(SecretRequestRecord {
            command_id: command.into(),
            thread_id: "t1".into(),
            run_id: run_id.as_str().into(),
            node_id: node.into(),
            turn_item_id: "turn-item:secret-request:t1:k".into(),
            label: "Deploy token".into(),
            reason: "To deploy".into(),
            url: None,
            placeholder: None,
            delivery: None,
            secret_status: status,
        }))
    };
    let card = |o: &Orchestrator| match o.store.record::<TurnItem>("t1", "turn-item:secret-request:t1:k").unwrap() {
        Some(TurnItem::SecretRequest(card)) => *card,
        other => panic!("{other:?}"),
    };
    let refused = o.dispatch(record("s0", "node:elsewhere", SecretRequestStatus::Pending)).await.unwrap_err();
    assert!(refused.to_string().contains("is not the root of run"), "{refused}");
    o.dispatch(record("s1", root.as_str(), SecretRequestStatus::Pending)).await.unwrap();
    assert_eq!((card(&o).secret_status, card(&o).status), (SecretRequestStatus::Pending, TurnItemStatus::Waiting));
    assert_eq!(card(&o).provider_turn_id.map(|t| t.as_str().to_owned()).as_deref(), Some("pturn-1"));
    o.dispatch(record("s2", root.as_str(), SecretRequestStatus::Saved)).await.unwrap();
    assert_eq!((card(&o).secret_status, card(&o).status), (SecretRequestStatus::Saved, TurnItemStatus::Completed));
    let before = o.store.latest_sequence().unwrap();
    o.dispatch(record("s3", root.as_str(), SecretRequestStatus::Cancelled)).await.unwrap();
    assert_eq!(o.store.latest_sequence().unwrap(), before, "an ended card stays as it ended");
    assert_eq!(card(&o).secret_status, SecretRequestStatus::Saved);
    // No client may record one.
    let sent = json!({ "type": "secret_request.record", "commandId": "x", "threadId": "t1", "runId": run_id, "nodeId": root, "turnItemId": "i2",
        "label": "l", "reason": "r", "secretStatus": "saved" });
    let refused = crate::rpc::shell::dispatch_command(&o, sent).await.unwrap_err();
    assert!(refused.to_string().contains("not supported"), "{refused}");
}
