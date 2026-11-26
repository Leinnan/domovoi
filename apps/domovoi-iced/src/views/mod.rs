//! Root layout: Connect view or Workspace (Top Bar | Resizable Sidebar | Main
//! Column | Resizable Side Panel | Status Bar) with layered popovers, modals,
//! Command Palette, and toasts.

pub mod companion;
pub mod composer;
mod connect;
pub mod image_viewer;
pub mod markdown;
mod palette;
pub mod panel;
pub mod provider_editor;
pub mod scheduled_editor;
mod pull_requests;
mod settings;
mod sidebar;
mod thread;
mod usage;

use domovoi_core::Event;
use iced::widget::{Column, Space, button, column, container, mouse_area, row, stack, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, Message, Overlay, ResizeSide, SettingsSection, now_ms};
use crate::ui::fonts;
use crate::ui::icons::{Icon, PROJECT_SYMBOLS, project_symbol};
use crate::ui::theme::{Appearance, PROJECT_KEYS, project_color};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = if app.show_main() && !app.connect.adding_server {
        workspace(app)
    } else {
        connect::view(app)
    };

    let mut layers = vec![body];

    if let Some(ov) = &app.overlay {
        layers.push(render_overlay(app, ov));
    }

    // Over overlays, so what the image viewer did ("Image copied") shows.
    if let Some(toast) = &app.vm.toast {
        layers.push(w::toast(
            &toast.kind,
            &toast.message,
            &toast.action_label,
            (!toast.action_label.is_empty()).then(|| core(Event::ToastAction)),
            core(Event::DismissToast),
        ));
    }

    if let Some(pal) = &app.vm.palette {
        layers.push(w::modal(palette::view(app, pal), core(Event::ClosePalette), w::Placement::Top(72.0)));
    }

    // Over everything, on any screen: a link from outside waits for its answer.
    if let Some(request) = &app.vm.connection.link_request {
        layers.push(connect::link_dialog(request));
    }

    stack(layers).into()
}

fn workspace(app: &App) -> Element<'_, Message> {
    let shows_panel = !matches!(app.vm.route.as_str(), "pull-requests" | "usage");
    let panel_open = app.vm.panel.open && shows_panel;

    let mut split = row![];
    if app.prefs.show_sidebar {
        split = split
            .push(container(sidebar::view(app)).width(app.prefs.sidebar_width).height(Length::Fill).style(w::surface))
            .push(resize_handle(ResizeSide::Sidebar));
    }

    let route_body: Element<'_, Message> = match app.vm.route.as_str() {
        "thread" => match &app.vm.thread {
            Some(t) => thread::thread_view(app, t),
            None => thread::new_thread_view(app),
        },
        "pull-requests" => pull_requests::view(app),
        "usage" => usage::view(app),
        _ => thread::new_thread_view(app),
    };

    let main_col = column![thread::status_strip(app), route_body].height(Length::Fill).width(Length::Fill);
    split = split.push(container(main_col).width(Length::Fill).height(Length::Fill).style(w::page));

    if panel_open {
        split = split
            .push(resize_handle(ResizeSide::Panel))
            .push(container(panel::view(app)).width(app.prefs.panel_width).height(Length::Fill).style(w::surface));
    }

    column![top_bar(app, shows_panel), w::divider(), split.height(Length::Fill), w::divider(), status_bar(app)]
        .height(Length::Fill)
        .into()
}

fn top_bar(app: &App, shows_panel: bool) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let connected = conn.state == "connected";
    let mood = companion::mood_for(connected, &conn.activity, app.companion_reaction.mood(now_ms()));
    let env_label = if conn.environment_label.is_empty() {
        "Domovoi"
    } else {
        conn.environment_label.as_str()
    };

    let server_btn = button(
        row![
            iced::widget::mouse_area(companion::view(conn.companion.look.clone(), mood, 24.0, app.tick_phase, app.companion_poked))
                .on_press(Message::PokeCompanion)
                .interaction(iced::mouse::Interaction::Pointer),
            text(env_label).size(TEXT_UI).font(fonts::SEMIBOLD),
            w::themed_icon(Icon::ChevronDown, ICON_XS, |c| c.muted),
        ]
        .spacing(TIGHT)
        .align_y(Alignment::Center),
    )
    .padding([4.0, S])
    .style(w::ghost_style)
    .on_press(Message::Overlay(Some(Overlay::ServerMenu)));

    let route_key: &str = match app.vm.route.as_str() {
        "pull-requests" => "pull-requests",
        "usage" => "usage",
        _ => "thread",
    };

    let route_tabs = w::segmented(
        &[
            ("thread", Some(Icon::MessageSquare), "Threads"),
            ("pull-requests", Some(Icon::GitPullRequest), "Pull Requests"),
            ("usage", Some(Icon::BarChart2), "Usage"),
        ],
        route_key,
        |r| match r {
            "pull-requests" => core(Event::ShowRoute { route: "pull-requests".into() }),
            "usage" => core(Event::ShowRoute { route: "usage".into() }),
            _ => core(Event::NewThread { project_id: None }),
        },
    );

    let nav = row![
        w::icon_button_tinted(
            Icon::PanelLeft,
            &format!("Toggle sidebar ({})", w::shortcut_label("Mod+B")),
            Some(Message::ToggleSidebar),
            app.prefs.show_sidebar,
        ),
        server_btn,
        Space::new().width(XS),
        route_tabs,
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    let right = row![
        button(
            row![
                w::themed_icon(Icon::Search, ICON_SMALL, |c| c.muted),
                w::muted("Command palette", TEXT_SMALL),
                w::kbd("Mod+K"),
            ]
            .spacing(S)
            .align_y(Alignment::Center)
        )
        .padding([4.0, S])
        .style(w::secondary_style)
        .on_press(core(Event::OpenPalette)),
        Space::new().width(XS),
        w::icon_button_tinted(
            Icon::PanelRight,
            &format!("Toggle side panel ({})", w::shortcut_label("Mod+J")),
            shows_panel.then(|| core(Event::TogglePanel)),
            app.vm.panel.open && shows_panel,
        ),
        w::icon_button(
            Icon::Settings,
            &format!("Settings ({})", w::shortcut_label("Mod+,")),
            Some(Message::OpenSettings(SettingsSection::General)),
        ),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    container(row![nav, Space::new().width(Length::Fill), right].align_y(Alignment::Center))
        .height(TOP_BAR)
        .padding([0.0, CHROME_PAD_X])
        .width(Length::Fill)
        .style(w::sunken)
        .into()
}

fn status_bar(app: &App) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let (dot_tone, conn_text): (w::Tone, String) = match conn.state.as_str() {
        "connected" => {
            let suffix = if conn.remote && !conn.path.is_empty() {
                format!(" · iroh {} ({} ms)", conn.path, conn.rtt_ms)
            } else if conn.remote {
                " · iroh".into()
            } else {
                String::new()
            };
            (|c| c.success, format!("Connected{suffix}"))
        }
        "reconnecting" => (|c| c.warning, "Reconnecting…".into()),
        "connecting" | "pairing" => (|c| c.info, "Connecting…".into()),
        _ => (|c| c.danger, "Disconnected".into()),
    };

    let mut left = row![
        w::dot(dot_tone, 7.0),
        w::muted(conn_text, TEXT_CAPTION),
    ]
    .spacing(TIGHT)
    .align_y(Alignment::Center);

    if !conn.server_version.is_empty() {
        left = left.push(w::faint(format!("v{}", conn.server_version), TEXT_CAPTION));
    }
    if conn.activity.working > 0 {
        left = left.push(w::badge_capsule(format!("{} running", conn.activity.working), Some(|c| c.success)));
    }
    if conn.activity.attention > 0 {
        left = left.push(w::badge_capsule(format!("{} needs you", conn.activity.attention), Some(|c| c.warning)));
    }

    let next_theme = if app.is_dark() { Appearance::Light } else { Appearance::Dark };
    let right = row![
        button(
            row![
                w::themed_icon(if app.is_dark() { Icon::Moon } else { Icon::Sun }, ICON_XS, |c| c.muted),
                w::faint(if app.is_dark() { "Hearth Dark" } else { "Hearth Light" }, TEXT_CAPTION),
            ]
            .spacing(XS)
            .align_y(Alignment::Center)
        )
        .padding([2.0, TIGHT])
        .style(w::ghost_style)
        .on_press(Message::Appearance(next_theme)),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    container(row![left, Space::new().width(Length::Fill), right].align_y(Alignment::Center))
        .height(STATUS_BAR)
        .padding([0.0, CHROME_PAD_X])
        .width(Length::Fill)
        .style(w::surface)
        .into()
}

fn resize_handle(side: ResizeSide) -> Element<'static, Message> {
    mouse_area(row![Space::new().width(2), w::vdivider(), Space::new().width(2)].height(Length::Fill))
        .on_press(Message::ResizeStart(side))
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .into()
}

fn render_overlay<'a>(app: &'a App, ov: &'a Overlay) -> Element<'a, Message> {
    let close = Message::Overlay(None);
    let content: Element<'a, Message> = match ov {
        // Full-window, with its own backdrop.
        Overlay::ImageViewer { images, index } => return image_viewer::view(app, images, *index),
        Overlay::ImageMenu { images, index } => image_viewer::menu(images, *index),
        Overlay::Settings(section) => settings::view(app, *section),
        Overlay::ServerMenu => {
            let conn = &app.vm.connection;
            let mut col = Column::new().spacing(XXS).push(w::section("Servers"));
            for env in &conn.environments {
                col = col.push(
                    w::list_row(
                        row![
                            w::themed_icon(if env.remote { Icon::Globe } else { Icon::Laptop }, ICON_SMALL, move |c| if env.active { c.accent } else { c.muted }),
                            column![
                                text(&env.label).size(TEXT_UI).font(if env.active { fonts::SEMIBOLD } else { fonts::INTER }),
                                w::faint(&env.detail, TEXT_CAPTION),
                            ]
                            .width(Length::Fill),
                            if env.active { w::themed_icon(Icon::Check, ICON_SMALL, |c| c.accent) } else { Space::new().into() },
                        ]
                        .spacing(S)
                        .align_y(Alignment::Center),
                        env.active,
                        true,
                    )
                    .on_press(core(Event::SelectEnvironment { id: env.id.clone() })),
                );
            }
            if let Some((idx, active)) = conn.environments.iter().enumerate().find(|(_, e)| e.active) {
                col = col.push(w::divider());
                if idx > 0 {
                    col = col.push(w::menu_item(Icon::ArrowUp, "Move Up", core(Event::MoveEnvironment { id: active.id.clone(), delta: -1 }), false));
                }
                if idx + 1 < conn.environments.len() {
                    col = col.push(w::menu_item(Icon::ArrowDown, "Move Down", core(Event::MoveEnvironment { id: active.id.clone(), delta: 1 }), false));
                }
                col = col
                    .push(w::menu_item(
                        Icon::Pencil,
                        format!("Rename {}…", active.label),
                        Message::Overlay(Some(Overlay::RenameEnvironment {
                            env_id: active.id.clone(),
                            value: active.label.clone(),
                        })),
                        false,
                    ))
                    .push(w::menu_item(
                        Icon::Trash2,
                        format!("Forget {}…", active.label),
                        Message::Overlay(Some(Overlay::ConfirmForgetServer {
                            env_id: active.id.clone(),
                            label: active.label.clone(),
                        })),
                        true,
                    ));
            }
            col = col
                .push(w::divider())
                .push(w::menu_item(Icon::Plus, "Add Server…", Message::ShowAddServer(true), false))
                .push(w::menu_item(Icon::Smile, "Customize Companion…", Message::OpenSettings(SettingsSection::Companion), false))
                .push(w::menu_item(Icon::Power, "Disconnect", core(Event::Disconnect), true));
            w::menu_dialog(col, 360.0).into()
        }
        Overlay::SidebarAddMenu => {
            let col = column![
                w::dialog_header(Some((Icon::FolderPlus, |c| c.accent)), "Workspace & Threads", Some(close.clone())),
                w::menu_item(
                    Icon::FolderPlus,
                    "New Project…",
                    Message::Overlay(Some(Overlay::NewProject { name: String::new(), path: String::new() })),
                    false,
                ),
                w::menu_item(
                    Icon::FolderOpen,
                    "Browse Server Folders…",
                    Message::Overlay(Some(Overlay::FolderBrowser)),
                    false,
                ),
                w::menu_item(
                    Icon::Download,
                    "Import Agent Sessions…",
                    Message::Overlay(Some(Overlay::ImportSessions { selected: Vec::new() })),
                    false,
                ),
                w::divider(),
                w::menu_item(Icon::MessageSquare, "New Scratch Thread", core(Event::OpenScratch), false),
            ]
            .spacing(XXS);
            w::menu_dialog(col, 320.0).into()
        }
        Overlay::ThreadMenu(tid) => {
            let found = find_thread_row(app, tid);
            let title = found.map(|t| t.title.clone()).unwrap_or_default();
            let pinned = found.is_some_and(|t| t.pinned);
            let settled = found.is_some_and(|t| t.settled);
            let archived = found.is_some_and(|t| t.archived);
            // Details only the open thread's page carries.
            let page = app.vm.thread.as_ref().filter(|t| t.id == *tid);
            let mut extra = Column::new().spacing(XXS);
            if let Some(p) = page.filter(|p| p.worktree_path.is_some() && p.project_title == "No project") {
                extra = extra.push(w::menu_item(Icon::FolderPlus, "Make This Folder a Project", core(Event::PromoteScratchThread { thread_id: p.id.clone() }), false));
            }
            if let Some(p) = page.filter(|p| !p.cwd.is_empty() && server_is_local(&app.vm.connection)) {
                extra = extra.push(w::menu_item(Icon::FolderOpen, "Reveal Working Folder", Message::OpenUrl(p.cwd.clone()), false));
            }
            extra = extra
                .push(w::menu_item(Icon::Copy, "Copy Thread ID", Message::CopyText(tid.clone()), false));
            if page.is_some() && !archived {
                extra = extra.push(w::menu_item(Icon::CalendarClock, "Schedule This Thread…", Message::ScheduleThread, false));
            }
            extra = extra
                .push(w::menu_item(Icon::Shield, "Forget Allowed Tools", core(Event::ForgetThreadPermissions { thread_id: tid.clone() }), false));
            let col = column![
                w::dialog_header(Some((Icon::MessageSquare, |c| c.accent)), if title.is_empty() { "Thread Actions".to_owned() } else { title.clone() }, Some(close.clone())),
                w::menu_item(Icon::Pencil, "Rename…", Message::Overlay(Some(Overlay::RenameThread { thread_id: tid.clone(), value: title.clone() })), false),
                w::menu_item(Icon::Pin, if pinned { "Unpin" } else { "Pin" }, core(Event::PinThread { thread_id: tid.clone(), pinned: !pinned }), false),
                w::menu_item(Icon::CircleCheck, if settled { "Mark Active" } else { "Settle" }, core(Event::SettleThread { thread_id: tid.clone(), settled: !settled }), false),
                w::menu_item(Icon::Clock, "Snooze…", Message::Overlay(Some(Overlay::SnoozeThread { thread_id: tid.clone(), title: title.clone() })), false),
                w::menu_item(Icon::Bell, "Mark Unread", core(Event::MarkThreadUnread { thread_id: tid.clone() }), false),
                w::menu_item(Icon::Sparkles, "Regenerate Title", core(Event::RegenerateTitle { thread_id: tid.clone() }), false),
                w::menu_item(Icon::GitBranch, "Fork Thread", core(Event::ForkThread { thread_id: tid.clone() }), false),
                w::menu_item(Icon::Archive, if archived { "Unarchive" } else { "Archive" }, core(Event::ArchiveThread { thread_id: tid.clone(), archived: !archived }), false),
                w::divider(),
                extra,
                w::divider(),
                w::menu_item(Icon::Trash2, "Delete Thread…", Message::Overlay(Some(Overlay::ConfirmDeleteThread { thread_id: tid.clone(), title })), true),
            ]
            .spacing(XXS);
            w::menu_dialog(col, 340.0).into()
        }
        Overlay::ProjectMenu(pid) => {
            let proj = app.vm.sidebar.projects.iter().find(|p| p.id == *pid);
            let title = proj.map(|p| p.title.clone()).unwrap_or_default();
            let mut col = Column::new().spacing(XXS).push(w::dialog_header(
                Some((Icon::Folder, |c| c.accent)),
                if title.is_empty() { "Project".to_owned() } else { title.clone() },
                Some(close.clone()),
            ));
            col = col
                .push(w::menu_item(Icon::Plus, "New Thread", core(Event::NewThread { project_id: Some(pid.clone()) }), false))
                .push(w::menu_item(
                    Icon::Pencil,
                    "Rename…",
                    Message::Overlay(Some(Overlay::RenameProject { project_id: pid.clone(), value: title.clone() })),
                    false,
                ));
            if let Some(p) = proj {
                col = col.push(w::menu_item(
                    Icon::Sparkles,
                    "Change Icon…",
                    Message::Overlay(Some(Overlay::ProjectIconPicker {
                        project_id: p.id.clone(),
                        kind: p.icon.kind.clone(),
                        text: p.icon.text.clone(),
                        symbol: p.icon.symbol.clone(),
                        color: p.icon.color.clone(),
                        file_query: String::new(),
                    })),
                    false,
                ));
                if !p.is_scratch {
                    col = col.push(w::divider()).push(w::menu_item(
                        Icon::Trash2,
                        "Remove Project…",
                        Message::Overlay(Some(Overlay::ConfirmRemoveProject { project_id: p.id.clone(), title })),
                        true,
                    ));
                }
            }
            w::menu_dialog(col, 340.0).into()
        }
        Overlay::RenameThread { thread_id, value } => {
            let tid = thread_id.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::Pencil, |c| c.accent)), "Rename Thread", Some(close.clone())),
                    w::input("Thread title", value)
                        .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::RenameThread { thread_id: tid.clone(), value: v })))
                        .on_submit(Message::SubmitOverlay),
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Rename", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                420.0,
            )
            .into()
        }
        Overlay::RenameProject { project_id, value } => {
            let pid = project_id.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::Pencil, |c| c.accent)), "Rename Project", Some(close.clone())),
                    w::input("Project name", value)
                        .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::RenameProject { project_id: pid.clone(), value: v })))
                        .on_submit(Message::SubmitOverlay),
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Rename", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                420.0,
            )
            .into()
        }
        Overlay::RenameEnvironment { env_id, value } => {
            let eid = env_id.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::Pencil, |c| c.accent)), "Rename Server", Some(close.clone())),
                    w::input("Server name", value)
                        .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::RenameEnvironment { env_id: eid.clone(), value: v })))
                        .on_submit(Message::SubmitOverlay),
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Rename", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                420.0,
            )
            .into()
        }
        Overlay::ConfirmDeleteThread { title, .. } => w::dialog(
            column![
                w::dialog_header(Some((Icon::Trash2, |c| c.danger)), format!("Delete “{title}”?"), None),
                w::muted("This permanently deletes the thread and its transcript from the server.", TEXT_UI),
                w::dialog_actions(vec![
                    w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                    w::label_button("Delete", w::danger_style, Some(Message::SubmitOverlay)).into(),
                ]),
            ]
            .spacing(M),
            440.0,
        )
        .into(),
        Overlay::ConfirmRemoveProject { title, .. } => w::dialog(
            column![
                w::dialog_header(Some((Icon::Trash2, |c| c.danger)), format!("Remove “{title}”?"), None),
                w::muted("Removes the project from Domovoi. Files on disk are untouched.", TEXT_UI),
                w::dialog_actions(vec![
                    w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                    w::label_button("Remove", w::danger_style, Some(Message::SubmitOverlay)).into(),
                ]),
            ]
            .spacing(M),
            440.0,
        )
        .into(),
        Overlay::ConfirmForgetServer { label, .. } => w::dialog(
            column![
                w::dialog_header(Some((Icon::Trash2, |c| c.danger)), format!("Forget “{label}”?"), None),
                w::muted("Signs this device out of the server and removes its saved credentials.", TEXT_UI),
                w::dialog_actions(vec![
                    w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                    w::label_button("Forget", w::danger_style, Some(Message::SubmitOverlay)).into(),
                ]),
            ]
            .spacing(M),
            440.0,
        )
        .into(),
        Overlay::ConfirmFullAccess => w::dialog(
            column![
                w::dialog_header(Some((Icon::Shield, |c| c.danger)), "Allow Full Access?", None),
                w::muted("The agent will run commands and edit files without asking for approval.", TEXT_UI),
                w::dialog_actions(vec![
                    w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                    w::label_button("Allow Full Access", w::danger_style, Some(Message::SubmitOverlay)).into(),
                ]),
            ]
            .spacing(M),
            440.0,
        )
        .into(),
        Overlay::SnoozeThread { thread_id, title } => {
            let now = now_ms();
            let presets = [
                ("1 hour", now + 3_600_000),
                ("3 hours", now + 3 * 3_600_000),
                ("Tomorrow (24h)", now + 24 * 3_600_000),
                ("Next week (7d)", now + 7 * 24 * 3_600_000),
                ("Wake now", 0),
            ];
            let mut col = Column::new().spacing(XS).push(w::dialog_header(
                Some((Icon::Clock, |c| c.violet)),
                format!("Snooze “{title}”"),
                Some(close.clone()),
            ));
            for (label, until_ms) in presets {
                col = col.push(
                    w::list_row(text(label).size(TEXT_UI), false, false)
                        .on_press(core(Event::SnoozeThread { thread_id: thread_id.clone(), until_ms })),
                );
            }
            w::menu_dialog(col, 360.0).into()
        }
        Overlay::NewProject { name, path } => {
            let p_clone = path.clone();
            let n_clone = name.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::FolderPlus, |c| c.accent)), "Add or Create Project", Some(close.clone())),
                    w::muted("Add an existing folder on the server by path, or create a new project folder by name.", TEXT_SMALL),
                    row![
                        w::input("Server folder path (e.g. /Users/me/code/app)", path)
                            .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::NewProject { name: n_clone.clone(), path: v })))
                            .width(Length::Fill),
                        w::small_button("Pick Local…", w::secondary_style, Some(Message::PickLocalProjectFolder)),
                    ]
                    .spacing(XS),
                    w::input("Or create new project name…", name)
                        .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::NewProject { name: v, path: p_clone.clone() })))
                        .on_submit(Message::SubmitOverlay),
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Add Project", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                500.0,
            )
            .into()
        }
        Overlay::FolderBrowser => {
            let fb = &app.vm.folder_browser;
            let mut list = Column::new().spacing(XXS);
            if !fb.parent_path.is_empty() {
                list = list.push(
                    w::list_row(row![w::themed_icon(Icon::ArrowLeft, ICON_SMALL, |c| c.muted), text("..").size(TEXT_UI)].spacing(S), false, false)
                        .on_press(core(Event::BrowseFolders { path: fb.parent_path.clone() })),
                );
            }
            for e in &fb.entries {
                list = list.push(
                    w::list_row(row![w::themed_icon(Icon::Folder, ICON_SMALL, |c| c.accent), text(&e.name).size(TEXT_UI)].spacing(S), false, false)
                        .on_press(core(Event::BrowseFolders { path: e.full_path.clone() })),
                );
            }
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::FolderOpen, |c| c.accent)), "Browse Server Folders", Some(close.clone())),
                    text(&fb.path).size(TEXT_SMALL).font(fonts::MONO),
                    if let Some(err) = &fb.error {
                        Element::from(w::colored(err, TEXT_SMALL, |c| c.danger))
                    } else {
                        w::scroll(list).height(Length::Fixed(300.0)).into()
                    },
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button(
                            if fb.is_project { "Already a Project" } else { "Add This Folder" },
                            w::primary_style,
                            (!fb.path.is_empty() && !fb.is_project).then(|| core(Event::AddProject { path: fb.path.clone() })),
                        )
                        .into(),
                    ]),
                ]
                .spacing(M),
                540.0,
            )
            .into()
        }
        Overlay::ImportSessions { selected } => {
            let ag = &app.vm.agent_sessions;
            let mut list = Column::new().spacing(XXS);
            for cand in &ag.candidates {
                let checked = selected.iter().any(|p| p == &cand.path);
                list = list.push(
                    w::list_row(
                        row![
                            w::themed_icon(
                                if checked { Icon::CircleCheck } else { Icon::CircleDot },
                                ICON_SMALL,
                                move |c| if checked { c.accent } else { c.muted },
                            ),
                            column![
                                text(&cand.title).size(TEXT_UI).font(fonts::MEDIUM),
                                w::muted(format!("{} · {}", cand.sources, cand.detail), TEXT_CAPTION),
                            ]
                            .width(Length::Fill),
                            if cand.already_imported { w::badge_capsule("Imported", None) } else { Space::new().into() },
                        ]
                        .spacing(S)
                        .align_y(Alignment::Center),
                        checked,
                        true,
                    )
                    .on_press(Message::ImportTogglePath(cand.path.clone())),
                );
            }
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::Download, |c| c.accent)), "Import Agent Sessions", Some(close.clone())),
                    w::muted("Import folders from Claude Code, Codex, or Pi on the server.", TEXT_SMALL),
                    if ag.scanning {
                        Element::from(w::muted("Scanning server…", TEXT_UI))
                    } else if ag.importing && ag.total > 0 {
                        column![
                            w::muted(format!("Importing {} of {}…", ag.done.min(ag.total), ag.total), TEXT_UI),
                            w::percent_bar(ag.done.min(ag.total) * 100 / ag.total, 0, |c| c.accent),
                        ]
                        .spacing(S)
                        .into()
                    } else {
                        w::scroll(list).height(Length::Fixed(320.0)).into()
                    },
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button(
                            format!("Import ({})", selected.len()),
                            w::primary_style,
                            (!selected.is_empty() && !ag.importing).then_some(Message::SubmitOverlay),
                        )
                        .into(),
                    ]),
                ]
                .spacing(M),
                560.0,
            )
            .into()
        }
        Overlay::ProjectIconPicker { project_id, kind, text: txt, symbol, color, file_query } => {
            let mut swatches = row![].spacing(XS);
            for &c_key in PROJECT_KEYS.iter().take(9) {
                let ov_next = Overlay::ProjectIconPicker {
                    project_id: project_id.clone(),
                    kind: kind.clone(),
                    text: txt.clone(),
                    symbol: symbol.clone(),
                    color: c_key.to_owned(),
                    file_query: file_query.clone(),
                };
                swatches = swatches.push(
                    button(Space::new().width(18.0).height(18.0))
                        .padding(2.0)
                        .style(move |t: &iced::Theme, _| button::Style {
                            background: Some(iced::Background::Color(project_color(c_key, crate::ui::theme::colors(t).dark))),
                            border: iced::Border { radius: RADIUS_PILL.into(), ..Default::default() },
                            ..Default::default()
                        })
                        .on_press(Message::UpdateOverlay(Box::new(ov_next))),
                );
            }
            let mut syms = row![].spacing(XS);
            for &sym_name in PROJECT_SYMBOLS.iter().take(12) {
                let ov_next = Overlay::ProjectIconPicker {
                    project_id: project_id.clone(),
                    kind: "symbol".into(),
                    text: txt.clone(),
                    symbol: sym_name.to_owned(),
                    color: color.clone(),
                    file_query: file_query.clone(),
                };
                syms = syms.push(w::icon_button_tinted(
                    project_symbol(sym_name),
                    sym_name,
                    Some(Message::UpdateOverlay(Box::new(ov_next))),
                    kind == "symbol" && symbol == sym_name,
                ));
            }
            let ov_for_text = ov.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::Sparkles, |c| c.accent)), "Project Icon", Some(close.clone())),
                    w::segmented(
                        &[("auto", None, "Auto"), ("text", None, "Monogram"), ("emoji", None, "Emoji"), ("symbol", None, "Symbol")],
                        kind.as_str(),
                        |k| {
                            let mut next = ov.clone();
                            if let Overlay::ProjectIconPicker { kind: ref mut kk, .. } = next {
                                *kk = k.to_owned();
                            }
                            Message::UpdateOverlay(Box::new(next))
                        },
                    ),
                    w::input("Monogram or emoji…", txt).on_input(move |v| {
                        let mut next = ov_for_text.clone();
                        if let Overlay::ProjectIconPicker { text: ref mut tt, .. } = next {
                            *tt = v;
                        }
                        Message::UpdateOverlay(Box::new(next))
                    }),
                    swatches,
                    syms,
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Save Icon", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                520.0,
            )
            .into()
        }
        Overlay::ScheduledTaskEditor(draft) => scheduled_editor::view(app, draft),
        Overlay::ProviderInstanceEditor(draft) => provider_editor::view(draft),
        Overlay::SavePlanDialog { item_id, path } => {
            let iid = item_id.clone();
            w::dialog(
                column![
                    w::dialog_header(Some((Icon::ClipboardList, |c| c.violet)), "Save Plan to Workspace", Some(close.clone())),
                    w::input("Relative path (blank for plans/<title>.md)", path)
                        .on_input(move |v| Message::UpdateOverlay(Box::new(Overlay::SavePlanDialog { item_id: iid.clone(), path: v })))
                        .on_submit(Message::SubmitOverlay),
                    w::dialog_actions(vec![
                        w::label_button("Cancel", w::secondary_style, Some(close.clone())).into(),
                        w::label_button("Save Plan", w::primary_style, Some(Message::SubmitOverlay)).into(),
                    ]),
                ]
                .spacing(M),
                440.0,
            )
            .into()
        }
        Overlay::ContextPopover => {
            let mut col = Column::new().spacing(S).push(w::dialog_header(
                Some((Icon::Cpu, |c| c.accent)),
                "Context & Session Usage",
                Some(close.clone()),
            ));
            let usage = app.vm.thread.as_ref().and_then(|t| t.usage.as_ref());
            if let Some(ctx) = usage.and_then(|u| u.context.as_ref()) {
                col = col.push(text(&ctx.label).size(TEXT_UI).font(fonts::SEMIBOLD));
                if !ctx.remaining_label.is_empty() {
                    col = col.push(w::muted(&ctx.remaining_label, TEXT_SMALL));
                }
                if !ctx.cost.is_empty() {
                    col = col.push(w::muted(format!("Cost: {}", ctx.cost), TEXT_SMALL));
                }
                for stat in &ctx.breakdown {
                    col = col.push(row![w::muted(&stat.label, TEXT_SMALL).width(Length::Fill), text(&stat.value).size(TEXT_SMALL).font(fonts::MONO)]);
                }
                if ctx.max_tokens > 0 {
                    let tone: w::Tone = if ctx.critical { |c| c.danger } else if ctx.high { |c| c.warning } else { |c| c.accent };
                    col = col.push(w::percent_bar(ctx.percent, ctx.compact_at_percent, tone));
                    if (1..100).contains(&ctx.compact_at_percent) {
                        col = col.push(w::faint(format!("Compacts automatically at {}%", ctx.compact_at_percent), TEXT_CAPTION));
                    }
                }
                if ctx.can_compact {
                    col = col.push(w::small_button("Compact Context (/compact)", w::primary_style, Some(core(Event::Compact))));
                }
            }
            if let Some((u, lim)) = usage.and_then(|u| u.limits.as_ref().map(|l| (u, l))) {
                if u.context.is_some() {
                    col = col.push(w::divider());
                }
                let checked = match u.limits_checked.as_str() {
                    "" => String::new(),
                    "now" => "Updated now".to_owned(),
                    ago => format!("Updated {ago} ago"),
                };
                col = col.push(row![text(format!("{} Limits", lim.provider)).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill), w::faint(checked, TEXT_CAPTION)].align_y(Alignment::Center));
                if !u.limits_notice.is_empty() {
                    col = col.push(w::muted(&u.limits_notice, TEXT_SMALL));
                }
                if !lim.windows.is_empty() {
                    col = col.push(w::limit_windows(&lim.windows));
                }
                if !lim.external_url.is_empty() {
                    let label = if lim.external_label.is_empty() { "Usage on the web" } else { lim.external_label.as_str() };
                    col = col.push(w::small_button(label, w::ghost_style, Some(Message::OpenUrl(lim.external_url.clone()))));
                }
            }
            col = col.push(w::divider()).push(w::small_button(
                "All Usage",
                w::secondary_style,
                Some(Message::ShowRoute("usage")),
            ));
            w::dialog(col, 400.0).into()
        }
    };

    w::modal(content, close, w::Placement::Center)
}

fn find_thread_row<'a>(app: &'a App, tid: &str) -> Option<&'a domovoi_core::view::ThreadRowView> {
    for sec in &app.vm.sidebar.sections {
        if let Some(t) = sec.rows.iter().find(|r| r.id == tid) {
            return Some(t);
        }
    }
    for p in &app.vm.sidebar.projects {
        if let Some(t) = p.threads.iter().find(|r| r.id == tid) {
            return Some(t);
        }
        if let Some(t) = p.earlier.iter().find(|r| r.id == tid) {
            return Some(t);
        }
    }
    app.vm.sidebar.snoozed.iter().find(|r| r.id == tid)
}

/// The server runs on this machine, so its folders are this machine's
/// (the file manager can show them).
fn server_is_local(conn: &domovoi_core::view::ConnectionView) -> bool {
    if conn.remote {
        return false;
    }
    let rest = conn.server_url.split_once("://").map_or(conn.server_url.as_str(), |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') { v6.split(']').next().unwrap_or("") } else { authority.split(':').next().unwrap_or("") };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}
