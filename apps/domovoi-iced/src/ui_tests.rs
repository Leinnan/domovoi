//! Headless UI integration & visual snapshot tests driven by `iced_test`.
//! Snapshots are written to `target/ui-snapshots/*.png`.

use std::sync::Arc;
use std::time::Duration;

use domovoi_core::companion::CompanionLook;
use domovoi_core::view::*;
use domovoi_core::Event;
use iced::Size;
use iced_test::simulator::Simulator;

use crate::app::{App, ComposerPopover, ConnectMode, Message, Overlay, SettingsSection, ViewerKey};
use crate::host::{self, HostMsg};
use domovoi_desktop_host::SecureStore;
use crate::ui::theme::Appearance;

fn snapshot(app: &App, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with(name) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let theme = app.theme();
    let mut ui = Simulator::with_size(crate::ui::fonts::settings(), Size::new(1440.0, 900.0), crate::views::view(app));
    let snap = ui.snapshot(&theme).unwrap();
    assert!(snap.matches_image(&path).unwrap());
}

fn click(app: &mut App, label: &str) {
    let messages: Vec<Message> = {
        let mut ui = Simulator::with_size(crate::ui::fonts::settings(), Size::new(1440.0, 900.0), crate::views::view(app));
        ui.click(label).unwrap();
        ui.into_messages().collect()
    };
    for m in messages {
        let _ = app.update(m);
    }
}

fn sample_connected_vm() -> ViewModel {
    let proj_icon = ProjectIconView {
        kind: "text".into(),
        text: "DM".into(),
        color: "amber".into(),
        project_id: "p1".into(),
        ..Default::default()
    };
    let row1 = ThreadRowView {
        project_icon: proj_icon.clone(),
        id: "t1".into(),
        project_id: "p1".into(),
        project_title: "t5code".into(),
        title: "Build cross-platform Iced client".into(),
        model_label: "Claude Opus".into(),
        status_label: "Needs you".into(),
        status: "approval".into(),
        unread: true,
        pinned: true,
        branch: Some("feat/iced-client".into()),
        age: "2m".into(),
        plan_ready: true,
        pull_request: "#42".into(),
        ..Default::default()
    };
    let row2 = ThreadRowView {
        project_icon: proj_icon.clone(),
        id: "t2".into(),
        project_id: "p1".into(),
        project_title: "t5code".into(),
        title: "Audit iroh relay reconnects".into(),
        model_label: "Codex".into(),
        status_label: "Working 1m".into(),
        status: "working".into(),
        age: "14m".into(),
        ..Default::default()
    };

    let look = CompanionLook {
        body: "amber".into(),
        eye_shape: "round".into(),
        pupil: "ink".into(),
        pupil_size: "medium".into(),
        antenna: true,
        ears: "round".into(),
        eyebrows: true,
        energy: "normal".into(),
        blink: "normal".into(),
        follow: true,
    };

    ViewModel {
        connection: ConnectionView {
            state: "connected".into(),
            server_url: "http://127.0.0.1:13773".into(),
            server_version: "0.1.0".into(),
            environment_label: "Mac Studio".into(),
            platform: "macOS arm64".into(),
            active_environment_id: "env1".into(),
            remote: true,
            path: "direct".into(),
            rtt_ms: 14,
            environments: vec![EnvironmentView {
                id: "env1".into(),
                label: "Mac Studio".into(),
                detail: "Remote · 3f2a9c".into(),
                remote: true,
                active: true,
                server_id: "srv1".into(),
                companion: CompanionLookView { look: look.clone(), custom: true },
            }],
            companion: CompanionLookView { look, custom: true },
            activity: ActivityView { working: 1, attention: 1, ..Default::default() },
            ..Default::default()
        },
        sidebar: SidebarView {
            grouping: "status".into(),
            sections: vec![
                SidebarSectionView {
                    kind: "inbox".into(),
                    title: "Inbox".into(),
                    count: 1,
                    expanded: true,
                    rows: vec![row1.clone()],
                    hidden_count: 0,
                },
                SidebarSectionView {
                    kind: "running".into(),
                    title: "Running".into(),
                    count: 1,
                    expanded: true,
                    rows: vec![row2.clone()],
                    hidden_count: 0,
                },
            ],
            projects: vec![SidebarProjectView {
                id: "p1".into(),
                title: "t5code".into(),
                workspace_root: "/Users/dev/t5code".into(),
                icon: proj_icon,
                expanded: true,
                threads: vec![row1, row2],
                ..Default::default()
            }],
            selected_thread_id: Some("t1".into()),
            selected_project_id: Some("p1".into()),
            threads_needing_attention: 1,
            ..Default::default()
        },
        route: "thread".into(),
        thread: Some(ThreadPageView {
            id: "t1".into(),
            title: "Build cross-platform Iced client".into(),
            project_id: "p1".into(),
            project_title: "t5code".into(),
            branch: Some("feat/iced-client".into()),
            model_label: "Claude Opus".into(),
            runtime_mode: "auto-accept-edits".into(),
            interaction_mode: "default".into(),
            status: "approval".into(),
            can_fork: true,
            artifact_count: 1,
            context: Some(ContextUsageView {
                used_tokens: 48_000,
                max_tokens: 200_000,
                percent: 24,
                label: "48k of 200k tokens".into(),
                remaining_label: "152k left".into(),
                cost: "$0.34".into(),
                can_compact: true,
                ..Default::default()
            }),
            items: vec![
                ItemView {
                    id: "u1".into(),
                    kind: "user".into(),
                    text: "Create the cross-platform Iced client using the Hearth palette.".into(),
                    ..Default::default()
                },
                ItemView {
                    id: "s1".into(),
                    kind: "command".into(),
                    verb: "Ran".into(),
                    icon: "command".into(),
                    title: "cargo test -p domovoi-iced".into(),
                    mono: true,
                    badge: "exit 0".into(),
                    group: "g1".into(),
                    group_summary: "Ran 1 command · edited 2 files".into(),
                    expandable: true,
                    output: Some(Arc::from("test result: ok. 12 passed; 0 failed")),
                    ..Default::default()
                },
                ItemView {
                    id: "a1".into(),
                    kind: "assistant".into(),
                    text: "Implemented `apps/domovoi-iced` with the Hearth palette.".into(),
                    blocks: Arc::new(vec![
                        MarkdownBlock {
                            kind: "heading".into(),
                            level: 2,
                            text: "Hearth Design System".into(),
                            ..Default::default()
                        },
                        MarkdownBlock {
                            kind: "paragraph".into(),
                            runs: vec![
                                MarkdownRun { text: "Built ".into(), ..Default::default() },
                                MarkdownRun { text: "apps/domovoi-iced".into(), code: true, ..Default::default() },
                                MarkdownRun { text: " driving ".into(), ..Default::default() },
                                MarkdownRun { text: "domovoi-core".into(), bold: true, ..Default::default() },
                                MarkdownRun { text: " directly in Rust.".into(), ..Default::default() },
                            ],
                            ..Default::default()
                        },
                        MarkdownBlock {
                            kind: "code".into(),
                            language: "rust".into(),
                            text: "pub fn theme(dark: bool) -> Theme {\n    hearth(dark)\n}".into(),
                            spans: vec![
                                CodeSpan { text: "pub fn ".into(), kind: "keyword".into() },
                                CodeSpan { text: "theme".into(), kind: "function".into() },
                                CodeSpan { text: "(dark: bool) -> ".into(), kind: "plain".into() },
                                CodeSpan { text: "Theme".into(), kind: "type".into() },
                                CodeSpan { text: " {\n    hearth(dark)\n}".into(), kind: "plain".into() },
                            ],
                            fence_closed: true,
                            ..Default::default()
                        },
                    ]),
                    can_fork_here: true,
                    run_duration_ms: 8_400,
                    ..Default::default()
                },
            ],
            pending_requests: vec![RequestView {
                id: "req1".into(),
                kind: "command".into(),
                prompt: "Run `cargo clippy --workspace`?".into(),
                detail: Some("cargo clippy --workspace --lib --bins".into()),
                session_label: "Allow for Thread".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        composer: ComposerView {
            placeholder: "Message the agent…".into(),
            selected_model: Some(ModelOption {
                instance_id: "claude".into(),
                model: "claude-opus".into(),
                label: "Claude Opus".into(),
            }),
            efforts: vec![
                ChoiceView { value: "low".into(), label: "Low".into() },
                ChoiceView { value: "high".into(), label: "High".into() },
            ],
            selected_effort: Some("high".into()),
            effort_label: "Effort".into(),
            runtime_modes: vec![
                ChoiceView { value: "approval-required".into(), label: "Ask".into() },
                ChoiceView { value: "auto-accept-edits".into(), label: "Auto-edit".into() },
                ChoiceView { value: "full-access".into(), label: "Full access".into() },
            ],
            runtime_mode: "auto-accept-edits".into(),
            interaction_mode: "default".into(),
            shows_interaction_mode: true,
            can_send: true,
            ..Default::default()
        },
        panel: PanelView {
            open: true,
            tab: "diff".into(),
            diff: DiffPanelView {
                sources: vec![ChoiceView { value: "working".into(), label: "Working tree".into() }],
                selected_source: "working".into(),
                files: vec![DiffFileView {
                    path: "apps/domovoi-iced/src/ui/theme.rs".into(),
                    change: "added".into(),
                    additions: 12,
                    deletions: 1,
                    hunks: vec![DiffHunkView {
                        header: "@@ -1,3 +1,5 @@".into(),
                        lines: vec![
                            DiffLineView { kind: "ctx".into(), old_line: Some(1), new_line: Some(1), text: "pub const DARK: Hearth = Hearth {".into() },
                            DiffLineView { kind: "del".into(), old_line: Some(2), new_line: None, text: "    accent: color!(0xE8A33D),".into() },
                            DiffLineView { kind: "add".into(), old_line: None, new_line: Some(2), text: "    accent: color!(0xF29932),".into() },
                        ],
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
            terminal: TerminalPanelView {
                can_rename: false,
                split: None,
                tabs: vec![TerminalTabView { id: "term1".into(), label: "zsh".into(), running: true, active: true }],
                active: Some(TerminalScreenView {
                    watching: false,
                    restored: false,
                    has_saved_screen: false,
                    id: "term1".into(),
                    rows: 24,
                    cols: 80,
                    lines: vec![
                        TerminalLineView {
                            spans: vec![
                                TerminalSpanView { text: "dev@hearth".into(), fg: 2, bg: -1, bold: true, ..Default::default() },
                                TerminalSpanView { text: " ~/t5code $ ".into(), fg: -1, bg: -1, ..Default::default() },
                                TerminalSpanView { text: "cargo test -p domovoi-iced".into(), fg: 6, bg: -1, ..Default::default() },
                            ],
                        },
                        TerminalLineView {
                            spans: vec![TerminalSpanView { text: "test result: ok. 4 passed".into(), fg: 10, bg: -1, ..Default::default() }],
                        },
                    ],
                    cursor_row: 2,
                    cursor_col: 0,
                    cursor_visible: true,
                    ..Default::default()
                }),
            },
            artifacts: ArtifactsPanelView {
                items: vec![ArtifactSummaryView {
                    id: "art1".into(),
                    title: "Hearth Badge".into(),
                    kind: "svg".into(),
                    latest_version: 1,
                    version_count: 1,
                    ..Default::default()
                }],
                active: Some(ActiveArtifactView {
                    id: "art1".into(),
                    title: "Hearth Badge".into(),
                    kind: "svg".into(),
                    version: 1,
                    mode: "preview".into(),
                    source: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"#F29932\"/></svg>".into(),
                    ..Default::default()
                }),
            },
            ..Default::default()
        },
        usage: UsageView {
            days: 7,
            range_label: "Last 7 days".into(),
            total_tokens: "14.8M".into(),
            total_cost: "$12.40".into(),
            cache_hit_label: "74%".into(),
            daily_avg_tokens: "2.1M/day".into(),
            sessions: 18,
            turns: 142,
            series: vec![
                UsageSeriesPointView {
                    key: "2026-11-20".into(),
                    label: "Nov 20".into(),
                    detail_label: "Nov 20, 2026".into(),
                    total_tokens: 3_200_000,
                    total_label: "3.2M".into(),
                    cost: "$2.80".into(),
                    by_provider: vec![UsageProviderPointView { provider_key: "claude".into(), provider: "Claude".into(), tokens: 3_200_000 }],
                },
                UsageSeriesPointView {
                    key: "2026-11-21".into(),
                    label: "Nov 21".into(),
                    detail_label: "Nov 21, 2026".into(),
                    total_tokens: 5_100_000,
                    total_label: "5.1M".into(),
                    cost: "$4.60".into(),
                    by_provider: vec![
                        UsageProviderPointView { provider_key: "claude".into(), provider: "Claude".into(), tokens: 3_100_000 },
                        UsageProviderPointView { provider_key: "codex".into(), provider: "Codex".into(), tokens: 2_000_000 },
                    ],
                },
            ],
            ..Default::default()
        },
        settings: SettingsView {
            remote: RemoteAccessView {
                available: true,
                enabled: true,
                running: true,
                endpoint_id: "3f2a9c8b7d6e5f4a".into(),
                state: "online".into(),
                link: "domovoi://connect?ticket=demo&code=1234-5678".into(),
                code: "1234-5678".into(),
                link_expires_at: "in 10m".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn desktop_ui_screens_and_snapshots() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;

    // 1. Connect screen (Dark + Light + Remote tab click)
    snapshot(&app, "01-connect-dark");
    click(&mut app, "Remote link (iroh)");
    assert_eq!(app.connect.mode, ConnectMode::Remote);
    app.prefs.appearance = Appearance::Light;
    snapshot(&app, "01b-connect-light");

    // 2. Main connected workspace with Diff panel (Dark + Light)
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    snapshot(&app, "02-workspace-diff-dark");
    app.prefs.appearance = Appearance::Light;
    snapshot(&app, "02b-workspace-diff-light");

    // 3. Terminal canvas & SVG Artifact panel
    app.prefs.appearance = Appearance::Dark;
    app.vm.panel.tab = "terminal".into();
    snapshot(&app, "03-terminal-dark");
    app.vm.panel.tab = "artifacts".into();
    snapshot(&app, "04-artifact-svg-dark");

    // 4. Usage page with stacked canvas chart
    app.vm.route = "usage".into();
    snapshot(&app, "05-usage-dark");

    // 5. Settings: Companion + Remote Access QR
    app.vm.route = "thread".into();
    app.pairing_qr = iced::widget::qr_code::Data::new(&app.vm.settings.remote.link)
        .ok()
        .map(|d| (app.vm.settings.remote.link.clone(), d));
    let _ = app.update(Message::OpenSettings(SettingsSection::Access));
    snapshot(&app, "06-settings-remote-qr");
    let _ = app.update(Message::OpenSettings(SettingsSection::Companion));
    snapshot(&app, "07-settings-companion");

    // 6. Composer popover & Command Palette
    let _ = app.update(Message::Overlay(None));
    let _ = app.update(Message::ComposerPopover(Some(ComposerPopover::Permissions)));
    assert_eq!(app.composer_popover, Some(ComposerPopover::Permissions));
}

#[test]
fn sidebar_row_marks_and_artifact_files() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();

    // Thread rows: a live working timer in plan mode, a merged PR, a fork in a worktree, and a snoozed thread.
    let sb = &mut app.vm.sidebar;
    sb.surface_threads = sb.sections[0].rows.clone();
    let inbox = &mut sb.sections[0].rows[0];
    inbox.pull_request_state = "merged".into();
    inbox.relation = "fork".into();
    inbox.in_worktree = true;
    let running = &mut sb.sections[1].rows[0];
    running.plan_mode = true;
    running.working_since_ms = crate::app::now_ms() - 3 * 60_000;
    let mut snoozed = running.clone();
    snoozed.id = "t3".into();
    snoozed.title = "Revisit relay metrics".into();
    snoozed.status = "idle".into();
    snoozed.status_label.clear();
    snoozed.working_since_ms = 0;
    snoozed.snoozed_until_ms = crate::app::now_ms() + 2 * 3_600_000;
    sb.sections[1].rows.push(snoozed);

    // Files: the workspace root; Artifacts: a streaming HTML file artifact, then the file it was written to.
    app.vm.panel.files.root = "/Users/dev/t5code".into();
    app.vm.panel.artifacts.active = None;
    app.vm.panel.artifacts.items.push(ArtifactSummaryView {
        id: "art2".into(),
        title: "Relay dashboard".into(),
        kind: "html".into(),
        latest_version: 1,
        version_count: 1,
        path: Some("dashboard.html".into()),
        streaming: true,
        ..Default::default()
    });
    app.vm.panel.tab = "files".into();
    snapshot(&app, "08-sidebar-marks-files-root");
    app.vm.panel.tab = "artifacts".into();
    snapshot(&app, "09-artifacts-streaming");
    app.vm.panel.files.artifact_preview = Some(FilePreviewView {
        path: "dashboard.html".into(),
        text: "<!doctype html>\n<title>Relay dashboard</title>".into(),
        ..Default::default()
    });
    snapshot(&app, "10-artifact-file-preview");

    // A Git LFS file whose object isn't on the server: its card offers Fetch,
    // and pressing it asks the core to fetch it.
    let oid = "4859402c258b836d02e955d1090e29f586e58b2040504d68afec3d8d43757bba";
    app.vm.panel.tab = "files".into();
    app.vm.panel.files.preview = Some(FilePreviewView {
        path: "assets/hero.png".into(),
        text: format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 2621440\n"),
        size_label: Some("2.5 MB".into()),
        lfs: Some(LfsView {
            oid: oid.into(),
            oid_short: oid[..10].into(),
            size_label: "2.5 MB".into(),
            state: "missing".into(),
            state_label: "Not downloaded".into(),
            message: Some("This file is kept in Git LFS, and its object isn't on the server yet.".into()),
            can_fetch: true,
            fetching: false,
        }),
        ..Default::default()
    });
    snapshot(&app, "11-files-lfs-not-downloaded");
    let mut ui = Simulator::with_size(crate::ui::fonts::settings(), Size::new(1440.0, 900.0), crate::views::view(&app));
    ui.click("Fetch").unwrap();
    let fetch = ui.into_messages().any(|m| matches!(m, Message::Core(e) if matches!(*e, Event::FetchLfsObject { ref path } if path == "assets/hero.png")));
    assert!(fetch, "Fetch asks the core to fetch the object");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crux_host_processes_launch_and_emits_deltas() {
    let store = Arc::new(SecureStore::in_memory());
    let (handle, mut rx) = host::start_test_host(store);

    // Initial full snapshot arrives immediately.
    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
    assert!(matches!(first, HostMsg::Delta(_)));

    handle.send(Event::Launch {
        surface: "desktop".into(),
        device_label: "TestHost".into(),
        device_os: "macOS".into(),
    });
    handle.send(Event::SetSearch { query: "hello".into() });

    let mut saw_search = false;
    let mut vm = ViewModel::default();
    for _ in 0..20 {
        if let Ok(Some(HostMsg::Delta(delta))) = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
            delta.apply(&mut vm);
            if vm.sidebar.search_query == "hello" {
                saw_search = true;
                break;
            }
        }
    }
    assert!(saw_search, "expected Search query delta from live domovoi-core");
    handle.shutdown();
}

#[test]
fn transcript_follows_scrolls_and_nudges() {
    use iced::mouse::{self, ScrollDelta};

    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    {
        let t = app.vm.thread.as_mut().unwrap();
        t.pending_requests.clear();
        for n in 0..60 {
            t.items.push(ItemView { id: format!("r{n}"), kind: "assistant".into(), text: format!("Reply {n}: checked the relay reconnect path again."), ..Default::default() });
        }
        t.working = true;
        t.status = "working".into();
        t.working_since_ms = crate::app::now_ms() - 83_000;
        t.working_activity = Some(WorkingActivityView { kind: Some("command".into()), step_item_id: Some("r59".into()) });
        t.background_tasks = (0..3)
            .map(|n| BackgroundTaskView { id: format!("b{n}"), kind: if n == 0 { "subagent".into() } else { "command".into() }, label: format!("Background job {n}"), status: if n == 1 { "waiting".into() } else { "running".into() }, progress: (n == 2).then(|| "12 of 40 files".into()), child_thread_id: (n == 0).then(|| "child".into()) })
            .collect();
        let window = |label: &str, percent: u32, elapsed: u32, pace: &str| LimitWindowView { label: label.into(), percent, resets: "in 3h".into(), high: percent >= 75, elapsed_percent: elapsed, pace: pace.into(), resets_at: "2026-11-23T20:00:00Z".into() };
        t.usage = Some(SessionUsageView {
            context: Some(ContextUsageView { used_tokens: 96_000, max_tokens: 200_000, percent: 48, label: "96k of 200k".into(), compact_at_percent: 80, ..Default::default() }),
            limits: Some(ProviderLimitsView { provider: "Claude".into(), windows: vec![window("5-hour", 82, 40, "ahead"), window("Weekly", 31, 55, "under")], ..Default::default() }),
            fullest: Some(window("5-hour", 82, 40, "ahead")),
            limit_alert: "caution".into(),
            limits_checked: "now".into(),
            ..Default::default()
        });
    }
    let _ = app.transcript.after_render::<Message>(app.vm.thread.as_ref());
    assert!(app.transcript.following);
    snapshot(&app, "11-working-background-usage");

    // Wheel up over the transcript: it stops following and offers the way back.
    let messages: Vec<Message> = {
        let mut ui = Simulator::with_size(crate::ui::fonts::settings(), Size::new(1440.0, 900.0), crate::views::view(&app));
        ui.point_at(iced::Point::new(880.0, 500.0));
        ui.simulate([iced::Event::Mouse(mouse::Event::WheelScrolled { delta: ScrollDelta::Lines { x: 0.0, y: 12.0 } })]);
        ui.into_messages().collect()
    };
    assert!(messages.iter().any(|m| matches!(m, Message::TranscriptScrolled(_))), "{messages:?}");
    for m in messages {
        let _ = app.update(m);
    }
    assert!(!app.transcript.following);
    snapshot(&app, "12-transcript-scrolled-up");

    // A new reply while reading above: a nudge, which jumps back down.
    app.vm.thread.as_mut().unwrap().items.push(ItemView { id: "r60".into(), kind: "assistant".into(), text: "Done.".into(), ..Default::default() });
    let _ = app.transcript.after_render::<Message>(app.vm.thread.as_ref());
    assert!(app.transcript.unseen);
    click(&mut app, "New messages");
    assert!(app.transcript.following && !app.transcript.unseen);
}

/// A small PNG, as a downloaded image's bytes.
fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        let pixels: Vec<u8> = (0..w * h).flat_map(|_| rgb).collect();
        writer.write_image_data(&pixels).unwrap();
    }
    out
}

fn loaded(app: &mut App, url: &str, rgb: [u8; 3]) {
    let _ = app.update(Message::Host(HostMsg::ImageLoaded { url: url.into(), handle: iced::widget::image::Handle::from_bytes(png_bytes(64, 48, rgb)) }));
}

fn shot(n: usize) -> ImageView {
    ImageView { name: format!("shot-{n}.png"), url: format!("http://127.0.0.1:13773/api/assets/tok/shot-{n}.png") }
}

fn viewer_index(app: &App) -> Option<usize> {
    match &app.overlay {
        Some(Overlay::ImageViewer { index, .. }) => Some(*index),
        _ => None,
    }
}

#[test]
fn transcript_images_open_in_the_viewer() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    let five: Vec<ImageView> = (0..5).map(shot).collect();
    let viewed = vec![shot(10), shot(11), shot(12)];
    {
        let t = app.vm.thread.as_mut().unwrap();
        t.pending_requests.clear();
        t.items = vec![
            ItemView { id: "u1".into(), kind: "user".into(), text: "These five screens look off".into(), images: five.clone(), ..Default::default() },
            ItemView { id: "s1".into(), kind: "tool".into(), icon: "image".into(), verb: "Viewed".into(), title: "shot-10.png".into(), group: "g1".into(), group_summary: "Viewed 3 images".into(), images: vec![shot(10)], group_images: viewed.clone(), ..Default::default() },
            ItemView { id: "s2".into(), kind: "tool".into(), icon: "image".into(), verb: "Viewed".into(), title: "shot-11.png".into(), group: "g1".into(), images: vec![shot(11), shot(12)], ..Default::default() },
        ];
    }
    let colors = [[220, 90, 70], [80, 160, 220], [90, 200, 120], [230, 190, 60], [170, 110, 220]];
    for (i, img) in five.iter().enumerate().take(4) {
        loaded(&mut app, &img.url, colors[i]);
    }
    // The fifth is still loading; one the steps viewed failed.
    loaded(&mut app, &viewed[0].url, colors[4]);
    loaded(&mut app, &viewed[1].url, colors[1]);
    let _ = app.update(Message::Host(HostMsg::ImageFailed { url: viewed[2].url.clone() }));
    assert!(app.failed_images.contains(&viewed[2].url));
    snapshot(&app, "13-transcript-images");

    // Three show; the third says two more, and opens the viewer on itself.
    click(&mut app, "+2");
    assert_eq!(viewer_index(&app), Some(2));
    snapshot(&app, "14-image-viewer");

    // ←/→ stop at the ends; Home/End and the filmstrip jump.
    let key = |app: &mut App, k: ViewerKey| {
        let _ = app.update(Message::ViewerKey(k));
    };
    key(&mut app, ViewerKey::Next);
    key(&mut app, ViewerKey::Next);
    key(&mut app, ViewerKey::Next);
    assert_eq!(viewer_index(&app), Some(4));
    key(&mut app, ViewerKey::First);
    key(&mut app, ViewerKey::Previous);
    assert_eq!(viewer_index(&app), Some(0));
    key(&mut app, ViewerKey::Last);
    assert_eq!(viewer_index(&app), Some(4));
    let _ = app.update(Message::ShowImage(1));
    assert_eq!(viewer_index(&app), Some(1));
    // The image still loading says so.
    let _ = app.update(Message::ShowImage(4));
    snapshot(&app, "15-image-viewer-loading");
    let _ = app.update(Message::Escape);
    assert!(app.overlay.is_none());
    // Closed, the keys do nothing.
    key(&mut app, ViewerKey::Next);
    assert!(app.overlay.is_none());

    // Open, a group shows each step's images instead of the strip, and the
    // failed one offers Retry, which asks again.
    let _ = app.update(Message::ToggleGroup("g1".into()));
    snapshot(&app, "16-step-images-retry");
    click(&mut app, "Retry");
    assert!(!app.failed_images.contains(&viewed[2].url));

    // A thumbnail's menu opens the viewer where it was.
    let _ = app.update(Message::Overlay(Some(Overlay::ImageMenu { images: five.clone(), index: 3 })));
    snapshot(&app, "17-image-menu");
    click(&mut app, "Open");
    assert_eq!(viewer_index(&app), Some(3));
}

#[test]
fn image_cache_lets_go_only_of_what_nothing_shows() {
    let (mut app, _) = App::new();
    app.vm = sample_connected_vm();
    let kept: Vec<ImageView> = (0..3).map(shot).collect();
    app.vm.thread.as_mut().unwrap().items.push(ItemView { id: "u9".into(), kind: "user".into(), images: kept.clone(), ..Default::default() });
    for img in &kept {
        loaded(&mut app, &img.url, [10, 10, 10]);
    }
    for n in 0..crate::app::IMAGE_CACHE_LIMIT {
        loaded(&mut app, &format!("http://127.0.0.1:13773/api/assets/tok/old-{n}.png"), [10, 10, 10]);
    }
    assert!(app.images.len() <= crate::app::IMAGE_CACHE_LIMIT);
    for img in &kept {
        assert!(app.images.contains_key(&img.url), "{} is still shown, so it stays", img.url);
    }
    assert!(!app.images.contains_key("http://127.0.0.1:13773/api/assets/tok/old-0.png"));
    assert!(app.wanted_media().contains(&kept[0].url));

    // A failed one that loads after all stops being failed.
    let _ = app.update(Message::Host(HostMsg::ImageFailed { url: kept[1].url.clone() }));
    loaded(&mut app, &kept[1].url, [10, 10, 10]);
    assert!(!app.failed_images.contains(&kept[1].url));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_image_fetch_can_be_asked_for_again() {
    let store = Arc::new(SecureStore::in_memory());
    let (handle, mut rx) = host::start_test_host(store);
    // Nothing listens on the discard port, so the fetch fails at once.
    let url = "http://127.0.0.1:9/api/assets/tok/a.png".to_owned();
    for attempt in 0..2 {
        handle.fetch_image(url.clone());
        let failed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match rx.recv().await {
                    Some(HostMsg::ImageFailed { url: u }) if u == url => return true,
                    Some(_) => {}
                    None => return false,
                }
            }
        })
        .await;
        assert_eq!(failed, Ok(true), "attempt {attempt}: the host reports the failure (and doesn't remember it)");
    }
    handle.shutdown();
}

fn messages_after_click(app: &App, label: &str) -> Vec<Message> {
    let mut ui = Simulator::with_size(crate::ui::fonts::settings(), Size::new(1440.0, 900.0), crate::views::view(app));
    ui.click(label).unwrap();
    ui.into_messages().collect()
}

#[test]
fn required_questions_hold_submit_until_answered() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    app.vm.thread.as_mut().unwrap().pending_requests = vec![RequestView {
        id: "req-q".into(),
        kind: "user_input".into(),
        prompt: "A few questions before I start".into(),
        questions: vec![
            QuestionView {
                id: "db".into(),
                header: "Storage".into(),
                question: "Which database should the cache use?".into(),
                required: true,
                options: vec![
                    QuestionOptionView { label: "SQLite".into(), value: "sqlite".into(), description: "One file next to the binary".into() },
                    QuestionOptionView { label: "Postgres".into(), value: "postgres".into(), description: String::new() },
                ],
                ..Default::default()
            },
            QuestionView { id: "notes".into(), question: "Anything else?".into(), allow_custom_answer: true, ..Default::default() },
        ],
        ..Default::default()
    }];
    snapshot(&app, "18-question-required-unanswered");

    // Nothing answered: Submit sends nothing, and neither does Enter in the optional field.
    let pressed = messages_after_click(&app, "Submit Answer");
    assert!(!pressed.iter().any(|m| matches!(m, Message::QuestionSubmit { .. })), "{pressed:?}");
    let _ = app.update(Message::QuestionCustomChanged { request_id: "req-q".into(), question_id: "notes".into(), text: "keep it small".into() });
    let _ = app.update(Message::QuestionSubmit { request_id: "req-q".into() });
    assert!(app.question_drafts.contains_key("req-q"), "an unanswered required question keeps the card");

    // Answering it lets Submit through, which hands over the answers.
    click(&mut app, "SQLite");
    snapshot(&app, "19-question-required-answered");
    let pressed = messages_after_click(&app, "Submit Answer");
    assert!(pressed.iter().any(|m| matches!(m, Message::QuestionSubmit { request_id } if request_id == "req-q")), "{pressed:?}");
    for m in pressed {
        let _ = app.update(m);
    }
    assert!(!app.question_drafts.contains_key("req-q"));
}

#[test]
fn plan_and_checklist_strip_and_turn_rows() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    let para = |t: &str| MarkdownBlock { kind: "paragraph".into(), runs: vec![MarkdownRun { text: t.into(), ..Default::default() }], ..Default::default() };
    let todo = |t: &str, s: &str| TodoView { text: t.into(), status: s.into() };
    {
        let t = app.vm.thread.as_mut().unwrap();
        t.pending_requests.clear();
        t.status = "idle".into();
        // Long enough that the open panel scrolls instead of pushing the transcript away.
        let mut blocks = vec![MarkdownBlock { kind: "heading".into(), level: 3, text: "Steps".into(), ..Default::default() }, para("Reconnect at once after a sleep, and keep the outbox while offline.")];
        blocks.extend((1..=14).map(|n| MarkdownBlock { kind: "list_item".into(), ordered: true, number: n, runs: vec![MarkdownRun { text: format!("Step {n} of the reconnect work"), ..Default::default() }], ..Default::default() }));
        t.plan = Some(PlanProgressView {
            plan_id: "plan-1".into(),
            label: "Plan: relay reconnects".into(),
            blocks: Arc::new(blocks),
            can_implement: true,
        });
        t.tasks = Some(TaskProgressView {
            done: 2,
            total: 4,
            current: "Write the tests".into(),
            label: "Tasks 2/4 · Write the tests".into(),
            previous_run: true,
            explanation: Some("From the last turn.".into()),
            todos: Arc::new(vec![todo("Find the reconnect path", "completed"), todo("Add the probe", "completed"), todo("Write the tests", "in_progress"), todo("Update the docs", "pending")]),
        });
        let started = crate::app::now_ms() - 95_000;
        t.items = vec![
            ItemView { id: "u1".into(), kind: "user".into(), text: "Make reconnects faster after sleep.".into(), ..Default::default() },
            ItemView { id: "c1".into(), kind: "command".into(), status: "completed".into(), verb: "Ran".into(), icon: "command".into(), title: "cargo test -p domovoi-core".into(), mono: true, exit_code: Some(0), ..Default::default() },
            ItemView { id: "f1".into(), kind: "file_change".into(), status: "completed".into(), verb: "Edited".into(), icon: "edit".into(), title: "dashboard.html".into(), mono: true, artifact_paths: vec!["dashboard.html".into()], ..Default::default() },
            ItemView { id: "a1".into(), kind: "assistant".into(), text: "Reconnects now start as soon as the machine wakes.".into(), run_started_ms: started, run_duration_ms: 83_000, ..Default::default() },
            ItemView {
                id: "cp1".into(),
                kind: "checkpoint".into(),
                title: "Turn 3".into(),
                checkpoint: Some(CheckpointView { id: "cp1".into(), turn_count: 3, label: "Checkpoint · turn 3".into(), files_changed: 2, additions: 41, deletions: 7, can_revert: true, ..Default::default() }),
                ..Default::default()
            },
        ];
    }
    snapshot(&app, "20-progress-strip-closed");

    // The strip opens on the plan and the to-dos; Implement carries it out and closes it.
    click(&mut app, "Plan: relay reconnects · Tasks 2/4 · Write the tests");
    assert_eq!(app.progress_open.as_deref(), Some("t1"));
    snapshot(&app, "21-progress-strip-open");
    let pressed = messages_after_click(&app, "Implement Plan");
    assert!(pressed.iter().any(|m| matches!(m, Message::ImplementPlan(id) if id == "plan-1")), "{pressed:?}");
    for m in pressed {
        let _ = app.update(m);
    }
    assert!(app.progress_open.is_none());

    // A checkpoint's diff is asked for by its turn when the core names no source.
    let pressed = messages_after_click(&app, "View diff");
    assert!(pressed.iter().any(|m| matches!(m, Message::ViewDiff(s) if s == "turn:3")), "{pressed:?}");
    // A page an edit wrote opens in the Artifacts panel.
    let pressed = messages_after_click(&app, "Open preview");
    assert!(pressed.iter().any(|m| matches!(m, Message::PreviewArtifactFile(p) if p == "dashboard.html")), "{pressed:?}");
}

#[test]
fn secret_requests_take_a_hidden_value_and_links_open_in_the_browser() {
    let (mut app, _) = App::new();
    app.prefs.appearance = Appearance::Dark;
    app.vm = sample_connected_vm();
    {
        let t = app.vm.thread.as_mut().unwrap();
        t.pending_requests.clear();
        t.secret_requests = vec![SecretRequestView {
            item_id: "s1".into(),
            label: "OpenAI API key".into(),
            reason: "To run the integration tests against the API.".into(),
            url: Some("https://platform.openai.com/api-keys".into()),
            placeholder: "sk-...".into(),
            delivery: "file".into(),
            hint: "Stored on the server; the agent reads it from a private file, gone after 24 hours.".into(),
            sending: false,
        }];
        t.links = vec![
            WorkLinkView { kind: "pull_request".into(), label: "#42".into(), title: "Iced client".into(), url: "https://github.com/o/r/pull/42".into(), state: "open".into() },
            WorkLinkView { kind: "issue".into(), label: "ENG-7".into(), title: "Desktop app".into(), url: "https://linear.app/o/issue/ENG-7".into(), state: String::new() },
        ];
    }
    snapshot(&app, "26-secret-request-empty");

    // Nothing typed: Save sends nothing.
    let pressed = messages_after_click(&app, "Save Secret");
    assert!(!pressed.iter().any(|m| matches!(m, Message::SecretSubmit { .. })), "{pressed:?}");

    // Typed: shown as dots, and never in a message's debug output.
    let _ = app.update(Message::SecretChanged { item_id: "s1".into(), value: crate::app::SecretText("sk-test-123".into()) });
    snapshot(&app, "27-secret-request-typed");
    let pressed = messages_after_click(&app, "Save Secret");
    assert!(pressed.iter().any(|m| matches!(m, Message::SecretSubmit { item_id } if item_id == "s1")), "{pressed:?}");
    let typed = Message::SecretChanged { item_id: "s1".into(), value: crate::app::SecretText("sk-test-123".into()) };
    assert!(!format!("{typed:?}").contains("sk-test-123"));
    for m in pressed {
        let _ = app.update(m);
    }
    assert!(!app.secret_drafts.contains_key("s1"), "sent, then forgotten here");

    // Decline goes to the core; the link to get one and the linked work open in the browser.
    let pressed = messages_after_click(&app, "Decline");
    assert!(pressed.iter().any(|m| matches!(m, Message::Core(e) if matches!(&**e, Event::DeclineSecret { item_id } if item_id == "s1"))), "{pressed:?}");
    let pressed = messages_after_click(&app, "Get one");
    assert!(pressed.iter().any(|m| matches!(m, Message::OpenUrl(u) if u == "https://platform.openai.com/api-keys")), "{pressed:?}");
    let pressed = messages_after_click(&app, "ENG-7");
    assert!(pressed.iter().any(|m| matches!(m, Message::OpenUrl(u) if u == "https://linear.app/o/issue/ENG-7")), "{pressed:?}");

    // Sending: the buttons wait; once the card is gone, nothing typed is kept.
    let _ = app.update(Message::SecretChanged { item_id: "s1".into(), value: crate::app::SecretText("again".into()) });
    app.vm.thread.as_mut().unwrap().secret_requests[0].sending = true;
    let pressed = messages_after_click(&app, "Saving…");
    assert!(!pressed.iter().any(|m| matches!(m, Message::SecretSubmit { .. })), "{pressed:?}");
}
