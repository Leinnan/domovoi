//! Settings modal covering desktop Appearance and every server settings section:
//! General, Providers (sign-in, install, instance editor), Keybindings,
//! Scheduled Tasks (runs, webhooks, editor), Archived, Projects (icon picker),
//! Maintenance (Storage + Diagnostics), Access (sessions + iroh Remote Access QR),
//! and Companion mascot customizer.

use domovoi_core::Event;
use domovoi_core::view::{EnvironmentView, ProjectSettingsView, ScheduledTaskView, ScheduledThreadChoiceView, WebhookDeliveryView};
use iced::widget::{Column, Space, button, column, container, qr_code, row, slider, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Theme};

use crate::app::{App, Message, Overlay, SettingsSection};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{Appearance, PROJECT_KEYS, colors, project_color};
use crate::ui::tokens::*;
use crate::ui::widgets as w;
use crate::views::companion;
use crate::views::provider_editor::ProviderDraft;
use crate::views::scheduled_editor::ScheduledDraft;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

const CLIENT_SECTIONS: [(SettingsSection, Icon, &str); 2] = [
    (SettingsSection::Appearance, Icon::Sun, "Appearance"),
    (SettingsSection::Servers, Icon::Server, "Servers"),
];

const SERVER_SECTIONS: [(SettingsSection, Icon, &str, bool); 9] = [
    (SettingsSection::General, Icon::Settings, "General", false),
    (SettingsSection::Providers, Icon::Cpu, "Providers", false),
    (SettingsSection::Projects, Icon::Folder, "Projects", false),
    (SettingsSection::Scheduled, Icon::CalendarClock, "Scheduled Tasks", false),
    (SettingsSection::Keybindings, Icon::Keyboard, "Keybindings", false),
    (SettingsSection::Access, Icon::Key, "Access & Remote", false),
    (SettingsSection::Archived, Icon::Archive, "Archived Threads", false),
    (SettingsSection::Maintenance, Icon::Wrench, "Maintenance", false),
    (SettingsSection::Companion, Icon::Smile, "Companion", true),
];

fn active_server(app: &App) -> Option<&EnvironmentView> {
    let conn = &app.vm.connection;
    conn.environments
        .iter()
        .find(|e| e.id == conn.active_environment_id)
        .or_else(|| conn.environments.iter().find(|e| e.active))
        .or_else(|| conn.environments.first())
}

pub fn view(app: &App, active: SettingsSection) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let connected = matches!(conn.state.as_str(), "connected" | "reconnecting");
    let server = active_server(app);
    let effective = if active.is_server_scoped() && server.is_none() {
        SettingsSection::Servers
    } else {
        active
    };

    let mut nav = Column::new().spacing(XS).padding(M).width(Length::Fixed(224.0));
    nav = nav.push(w::section("THIS DEVICE"));
    for (sec, ic, label) in CLIENT_SECTIONS {
        let selected = effective == sec;
        nav = nav.push(
            w::list_row(
                row![
                    w::themed_icon(ic, ICON_SMALL, move |c| if selected { c.accent } else { c.muted }),
                    text(label).size(TEXT_UI).font(if selected { fonts::SEMIBOLD } else { fonts::INTER }),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
                selected,
                false,
            )
            .on_press(Message::OpenSettings(sec)),
        );
    }

    nav = nav.push(Space::new().height(XS)).push(w::divider()).push(Space::new().height(XS));
    nav = nav.push(w::section("SERVER"));

    if let Some(srv) = server {
        nav = nav.push(server_switcher(app, srv));
        if app.settings_server_picker_open {
            nav = nav.push(server_switcher_list(app, &srv.id));
        }
        nav = nav.push(Space::new().height(XXS));
        for (sec, ic, label, is_local) in SERVER_SECTIONS {
            let selected = effective == sec;
            let mut r = row![
                w::themed_icon(ic, ICON_SMALL, move |c| if selected { c.accent } else { c.muted }),
                text(label).size(TEXT_UI).font(if selected { fonts::SEMIBOLD } else { fonts::INTER }).width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if is_local {
                r = r.push(w::faint("Local", TEXT_CAPTION));
            }
            nav = nav.push(w::list_row(r, selected, false).on_press(Message::OpenSettings(sec)));
        }
    } else {
        nav = nav.push(
            container(
                column![
                    w::muted("No servers paired yet.", TEXT_SMALL),
                    w::small_button("Add Server…", w::primary_style, Some(Message::ShowAddServer(true))),
                ]
                .spacing(S),
            )
            .padding(S)
            .width(Length::Fill)
            .style(w::inset_sunken),
        );
    }

    let content: Element<'_, Message> = if effective.requires_connection() && !connected {
        server_connection_placeholder(app, server, effective)
    } else {
        match effective {
            SettingsSection::Appearance => appearance_tab(app),
            SettingsSection::Servers => servers_tab(app),
            SettingsSection::General => general_tab(app),
            SettingsSection::Providers => providers_tab(app),
            SettingsSection::Keybindings => keybindings_tab(app),
            SettingsSection::Scheduled => scheduled_tab(app),
            SettingsSection::Archived => archived_tab(app),
            SettingsSection::Projects => projects_tab(app),
            SettingsSection::Maintenance => maintenance_tab(app),
            SettingsSection::Access => access_tab(app),
            SettingsSection::Companion => companion_tab(app),
        }
    };

    let pane = column![
        detail_header(app, effective, server),
        w::divider(),
        container(w::scroll(container(content).padding(CONTENT_GUTTER)).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    container(
        column![
            container(w::dialog_header(Some((Icon::Settings, |c| c.accent)), "Settings", Some(Message::Overlay(None))))
                .padding([M, L]),
            w::divider(),
            row![
                container(w::scroll(nav).height(Length::Fill)).height(Length::Fill).style(w::surface),
                w::vdivider(),
                container(pane).width(Length::Fill).height(Length::Fill).style(w::page),
            ]
            .height(Length::Fill),
        ],
    )
    .width(920)
    .height(660)
    .style(w::popover_style)
    .into()
}

fn server_status_tone(state: &str) -> w::Tone {
    match state {
        "connected" => |c| c.success,
        "connecting" | "reconnecting" | "pairing" => |c| c.warning,
        _ => |c| c.muted,
    }
}

fn server_status_short(app: &App) -> String {
    let conn = &app.vm.connection;
    match conn.state.as_str() {
        "connected" => {
            if conn.remote && !conn.path.is_empty() {
                format!("{} · {} ms", conn.path, conn.rtt_ms)
            } else {
                "Connected".into()
            }
        }
        "connecting" | "pairing" => "Connecting…".into(),
        "reconnecting" => "Reconnecting…".into(),
        _ => "Offline".into(),
    }
}

fn server_switcher<'a>(app: &'a App, srv: &'a EnvironmentView) -> Element<'a, Message> {
    let connected = app.vm.connection.state == "connected";
    let mood = if connected { companion::Mood::Idle } else { companion::Mood::Offline };
    let tone = server_status_tone(&app.vm.connection.state);
    let chevron = if app.settings_server_picker_open { Icon::ChevronUp } else { Icon::ChevronDown };
    let body = row![
        companion::view(srv.companion.look.clone(), mood, 22.0, app.tick_phase, false),
        column![
            text(&srv.label).size(TEXT_SMALL).font(fonts::SEMIBOLD),
            row![w::dot(tone, 6.0), w::faint(server_status_short(app), TEXT_CAPTION)]
                .spacing(XS)
                .align_y(Alignment::Center),
        ]
        .spacing(1.0)
        .width(Length::Fill),
        w::themed_icon(chevron, ICON_XS, |c| c.muted),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    container(
        button(body)
            .width(Length::Fill)
            .padding([TIGHT, S])
            .style(w::ghost_style)
            .on_press(Message::ToggleSettingsServerPicker),
    )
    .width(Length::Fill)
    .style(w::inset_surface)
    .into()
}

fn server_switcher_list<'a>(app: &'a App, current_id: &str) -> Element<'a, Message> {
    let mut list = Column::new().spacing(XXS);
    for env in &app.vm.connection.environments {
        let is_current = env.id == current_id;
        list = list.push(
            w::list_row(
                row![
                    companion::view(env.companion.look.clone(), companion::Mood::Idle, 18.0, 0.0, false),
                    text(&env.label)
                        .size(TEXT_SMALL)
                        .font(if is_current { fonts::SEMIBOLD } else { fonts::INTER })
                        .width(Length::Fill),
                    if is_current {
                        w::themed_icon(Icon::Check, ICON_XS, |c| c.accent)
                    } else {
                        Space::new().into()
                    },
                ]
                .spacing(S)
                .align_y(Alignment::Center),
                is_current,
                false,
            )
            .on_press(Message::SelectSettingsServer(env.id.clone())),
        );
    }
    list = list.push(w::divider()).push(
        w::list_row(
            row![w::themed_icon(Icon::Plus, ICON_XS, |c| c.muted), text("Add Server…").size(TEXT_SMALL)]
                .spacing(S)
                .align_y(Alignment::Center),
            false,
            false,
        )
        .on_press(Message::ShowAddServer(true)),
    );
    container(list).padding(XS).width(Length::Fill).style(w::inset_sunken).into()
}

fn detail_header<'a>(app: &'a App, sec: SettingsSection, srv: Option<&'a EnvironmentView>) -> Element<'a, Message> {
    let conn = &app.vm.connection;
    let connected = conn.state == "connected";
    let (badge_label, badge_tone): (&str, Option<w::Tone>) = if !sec.is_server_scoped() {
        ("This device", None)
    } else if sec == SettingsSection::Companion {
        ("Saved on this device", None)
    } else {
        ("Server setting", Some(|c| c.accent))
    };
    let subtitle = if !sec.is_server_scoped() {
        "Saved on this device only. Applies across all servers.".to_owned()
    } else if let Some(s) = srv {
        if sec == SettingsSection::Companion {
            format!("Customizes how {} looks on this device only.", s.label)
        } else {
            format!("Applies on {} across all connected devices.", s.label)
        }
    } else {
        "No server selected.".to_owned()
    };

    let leading: Element<'a, Message> = if sec.is_server_scoped() {
        if let Some(s) = srv {
            companion::view(
                s.companion.look.clone(),
                if connected { companion::Mood::Idle } else { companion::Mood::Offline },
                26.0,
                app.tick_phase,
                false,
            )
        } else {
            w::themed_icon(Icon::Server, ICON_LARGE, |c| c.muted)
        }
    } else {
        w::themed_icon(if sec == SettingsSection::Appearance { Icon::Sun } else { Icon::Server }, ICON_LARGE, |c| c.muted)
    };

    let mut header = row![
        leading,
        column![
            row![
                text(sec.title()).size(TEXT_BODY).font(fonts::SEMIBOLD),
                w::badge_capsule(badge_label, badge_tone),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
            w::muted(subtitle, TEXT_CAPTION),
        ]
        .spacing(XXS)
        .width(Length::Fill),
    ]
    .spacing(M)
    .align_y(Alignment::Center);

    if sec.is_server_scoped() && srv.is_some() {
        let state_badge: Element<'a, Message> = if connected {
            w::badge_capsule(server_status_short(app), Some(|c| c.success))
        } else if matches!(conn.state.as_str(), "connecting" | "reconnecting" | "pairing") {
            w::badge_capsule(server_status_short(app), Some(|c| c.warning))
        } else {
            w::small_button("Reconnect", w::primary_style, Some(core(Event::Reconnect))).into()
        };
        header = header.push(state_badge);
    }

    container(header).padding([M, CONTENT_GUTTER]).width(Length::Fill).style(w::surface).into()
}

fn server_connection_placeholder<'a>(app: &'a App, srv: Option<&'a EnvironmentView>, sec: SettingsSection) -> Element<'a, Message> {
    let conn = &app.vm.connection;
    let name = srv.map_or("this server", |s| s.label.as_str());
    let connecting = matches!(conn.state.as_str(), "connecting" | "reconnecting" | "pairing");
    let mut inner = Column::new().spacing(S).align_x(Alignment::Center);
    inner = inner
        .push(w::themed_icon(if connecting { Icon::RefreshCw } else { Icon::WifiOff }, 28.0, |c| c.muted))
        .push(
            text(if connecting { format!("Connecting to {name}…") } else { format!("{name} is offline") })
                .size(TEXT_BODY)
                .font(fonts::SEMIBOLD),
        )
        .push(w::muted(
            if connecting {
                format!("Loading {} from {name}.", sec.title().to_lowercase())
            } else if let Some(err) = conn.error.as_deref().filter(|e| !e.is_empty()) {
                err.to_owned()
            } else {
                format!("{} is stored on {name}. Reconnect to view or change it.", sec.title())
            },
            TEXT_SMALL,
        ));
    if !connecting {
        inner = inner.push(Space::new().height(XS)).push(w::small_button(
            "Reconnect",
            w::primary_style,
            Some(core(Event::Reconnect)),
        ));
    }
    container(inner)
        .padding(XL)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .style(w::inset_sunken)
        .into()
}

fn appearance_tab(app: &App) -> Element<'_, Message> {
    column![
        w::card(
            column![
                w::heading("Theme"),
                w::segmented(
                    &[
                        (Appearance::System, Some(Icon::Monitor), "System"),
                        (Appearance::Light, Some(Icon::Sun), "Hearth Light"),
                        (Appearance::Dark, Some(Icon::Moon), "Hearth Dark"),
                    ],
                    app.prefs.appearance,
                    Message::Appearance,
                ),
                Space::new().height(S),
                w::heading("Transcript text size"),
                row![
                    slider(TEXT_MIN..=TEXT_MAX, app.prefs.text_size, Message::TextSize).step(1.0_f32).width(Length::Fixed(240.0)),
                    text(format!("{} px", app.prefs.text_size.round())).size(TEXT_UI).font(fonts::MONO),
                ]
                .spacing(M)
                .align_y(Alignment::Center),
                w::muted("Uses Domovoi's Hearth palette with bundled Inter and JetBrains Mono fonts.", TEXT_SMALL),
            ]
            .spacing(S),
        )
        .width(Length::Fill),
    ]
    .spacing(L)
    .into()
}

fn servers_tab(app: &App) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let mut col = Column::new().spacing(L);

    if let Some(local) = &conn.local_server {
        col = col.push(
            w::card(
                row![
                    w::themed_icon(Icon::Monitor, ICON_LARGE, |c| c.accent),
                    column![
                        text(&local.label).size(TEXT_UI).font(fonts::SEMIBOLD),
                        w::muted(format!("{} · Server on this machine (no pairing code needed)", local.url), TEXT_CAPTION),
                    ]
                    .spacing(XXS)
                    .width(Length::Fill),
                    w::small_button("Connect", w::primary_style, Some(core(Event::ConnectLocalServer))),
                ]
                .spacing(M)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill),
        );
    }

    let mut list_col = Column::new().spacing(S).push(
        row![
            w::heading("Saved Servers"),
            Space::new().width(Length::Fill),
            w::icon_label_button(Icon::Plus, "Add Server…", w::Kind::Primary, Some(Message::ShowAddServer(true))),
        ]
        .align_y(Alignment::Center),
    );
    list_col = list_col.push(w::muted(
        "Server names and order are saved on this device. Choose Configure to edit settings stored on a specific server.",
        TEXT_CAPTION,
    ));

    if conn.environments.is_empty() {
        list_col = list_col.push(w::faint("No servers paired with this device yet.", TEXT_UI));
    }

    let count = conn.environments.len();
    for (idx, env) in conn.environments.iter().enumerate() {
        let is_connected = env.active && conn.state == "connected";
        let mood = if is_connected { companion::Mood::Idle } else { companion::Mood::Offline };
        let mut title_row = row![text(&env.label).size(TEXT_UI).font(if env.active { fonts::SEMIBOLD } else { fonts::MEDIUM })]
            .spacing(S)
            .align_y(Alignment::Center);
        if env.active {
            title_row = title_row.push(w::badge_capsule(server_status_short(app), Some(server_status_tone(&conn.state))));
        }

        let mut actions = row![].spacing(XS).align_y(Alignment::Center);
        if !env.active {
            actions = actions.push(w::small_button("Switch", w::ghost_style, Some(Message::SelectSettingsServer(env.id.clone()))));
        }
        actions = actions.push(w::small_button("Configure", w::secondary_style, Some(Message::ConfigureServer(env.id.clone()))));
        actions = actions.push(w::icon_button(
            Icon::Pencil,
            "Rename on this device",
            Some(Message::Overlay(Some(Overlay::RenameEnvironment {
                env_id: env.id.clone(),
                value: env.label.clone(),
            }))),
        ));
        if idx > 0 {
            actions = actions.push(w::icon_button(
                Icon::ArrowUp,
                "Move up",
                Some(core(Event::MoveEnvironment { id: env.id.clone(), delta: -1 })),
            ));
        }
        if idx + 1 < count {
            actions = actions.push(w::icon_button(
                Icon::ArrowDown,
                "Move down",
                Some(core(Event::MoveEnvironment { id: env.id.clone(), delta: 1 })),
            ));
        }
        actions = actions.push(w::icon_button(
            Icon::Trash2,
            "Forget server",
            Some(Message::Overlay(Some(Overlay::ConfirmForgetServer {
                env_id: env.id.clone(),
                label: env.label.clone(),
            }))),
        ));

        list_col = list_col.push(
            container(
                row![
                    companion::view(env.companion.look.clone(), mood, 28.0, 0.0, false),
                    column![title_row, w::faint(&env.detail, TEXT_CAPTION)].spacing(XXS).width(Length::Fill),
                    actions,
                ]
                .spacing(M)
                .align_y(Alignment::Center),
            )
            .padding(M)
            .width(Length::Fill)
            .style(w::inset_sunken),
        );
    }

    col = col.push(w::card(list_col).width(Length::Fill));
    col.into()
}

fn general_tab(app: &App) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let mut col = Column::new().spacing(L).push(
        w::card(
            column![
                w::heading("Connected Server"),
                kv_row("Address", &conn.server_url),
                kv_row("Environment", &conn.environment_label),
                kv_row("Version", &conn.server_version),
                kv_row("Platform", &conn.platform),
                Space::new().height(XS),
                row![
                    w::small_button("Disconnect", w::danger_style, Some(core(Event::Disconnect))),
                    w::small_button("Resync", w::secondary_style, Some(core(Event::Resync))),
                ]
                .spacing(S),
            ]
            .spacing(S),
        )
        .width(Length::Fill),
    );

    for grp in &app.vm.settings.groups {
        let mut g_col = Column::new().spacing(S).push(w::heading(&grp.title));
        for field in &grp.fields {
            let control: Element<'_, Message> = match field.kind.as_str() {
                "bool" => w::small_button(
                    if field.bool_value { "Enabled" } else { "Disabled" },
                    if field.bool_value { w::primary_style } else { w::secondary_style },
                    Some(core(Event::SetSettingBool { key: field.key.clone(), value: !field.bool_value })),
                )
                .into(),
                "choice" => {
                    let mut r = row![].spacing(XS);
                    for ch in &field.choices {
                        let active = ch.value == field.text_value;
                        r = r.push(w::small_button(
                            &ch.label,
                            if active { w::primary_style } else { w::secondary_style },
                            Some(core(Event::SetSettingText { key: field.key.clone(), value: ch.value.clone() })),
                        ));
                    }
                    r.into()
                }
                _ => {
                    let draft = app.setting_text_drafts.get(&field.key).map(String::as_str).unwrap_or(&field.text_value);
                    let k = field.key.clone();
                    let k2 = field.key.clone();
                    let val_save = draft.to_owned();
                    row![
                        w::input("", draft)
                            .on_input(move |v| Message::SettingTextChanged { key: k.clone(), value: v })
                            .width(Length::Fixed(200.0)),
                        w::small_button(
                            "Save",
                            w::secondary_style,
                            Some(core(Event::SetSettingText { key: k2, value: val_save })),
                        ),
                    ]
                    .spacing(XS)
                    .into()
                }
            };
            let mut label_col = Column::new()
                .spacing(XXS)
                .width(Length::Fill)
                .push(text(&field.label).size(TEXT_UI).font(fonts::MEDIUM));
            if !field.help.is_empty() {
                label_col = label_col.push(w::muted(&field.help, TEXT_CAPTION));
            }
            g_col = g_col.push(
                row![label_col, control]
                    .spacing(M)
                    .align_y(Alignment::Center),
            );
        }
        col = col.push(w::card(g_col).width(Length::Fill));
    }
    col.into()
}

fn providers_tab(app: &App) -> Element<'_, Message> {
    let mut col = Column::new().spacing(M);
    let mut bar = row![
        w::small_button("Check Again", w::secondary_style, Some(core(Event::RefreshProviders))),
    ]
    .spacing(S);
    if app.vm.settings.can_add_antigravity {
        bar = bar.push(w::small_button(
            "Add Antigravity Account…",
            w::primary_style,
            Some(Message::Overlay(Some(Overlay::ProviderInstanceEditor(Box::new(ProviderDraft::new_antigravity()))))),
        ));
    }
    col = col.push(bar);

    for p in &app.vm.settings.providers {
        let ready = p.status == "ready";
        let mut p_col = Column::new().spacing(XS).push(
            row![
                text(if p.name.is_empty() { &p.instance_id } else { &p.name })
                    .size(TEXT_BODY)
                    .font(fonts::SEMIBOLD)
                    .width(Length::Fill),
                w::badge_capsule(&p.status, Some(if ready { |c| c.success } else { |c| c.warning })),
            ]
            .align_y(Alignment::Center),
        );
        if !p.version.is_empty() {
            p_col = p_col.push(kv_row("Version", &p.version));
        }
        if !p.install.installed_version.is_empty() {
            p_col = p_col.push(kv_row("Installed runtime", &p.install.installed_version));
        }
        if p.config.editable && p.config.auth_methods.len() > 1 {
            let method = p.config.auth_methods.iter().find(|m| m.value == p.config.auth_method).map_or(p.config.auth_method.as_str(), |m| m.label.as_str());
            p_col = p_col.push(kv_row("Sign-in method", method));
        }
        if p.config.has_api_key {
            p_col = p_col.push(kv_row("API key", "Saved"));
        }
        if !p.auth.is_empty() {
            p_col = p_col.push(kv_row("Account", &p.auth));
        }
        p_col = p_col.push(kv_row("Models", p.model_count.to_string()));
        if !p.limits.is_empty() {
            p_col = p_col.push(kv_row("Limits", &p.limits));
        }
        if !p.message.is_empty() {
            p_col = p_col.push(w::muted(&p.message, TEXT_SMALL));
        }

        // Action buttons: Sign in / Install / Edit
        let mut acts = row![].spacing(XS);
        if p.can_authenticate && !p.sign_in.busy {
            acts = acts.push(w::small_button(
                if p.signed_in { "Sign in again" } else { "Sign in" },
                w::primary_style,
                Some(core(Event::StartProviderSignIn { instance_id: p.instance_id.clone() })),
            ));
        }
        if p.sign_in.busy {
            acts = acts.push(w::small_button(
                "Cancel sign-in",
                w::danger_style,
                Some(core(Event::CancelProviderSignIn { instance_id: p.instance_id.clone() })),
            ));
        }
        if p.can_install && !p.install.busy {
            let label = if p.install.update_available {
                format!("Update to {}", p.install.offered_version)
            } else {
                "Install runtime".into()
            };
            acts = acts.push(w::small_button(
                label,
                w::secondary_style,
                Some(core(Event::InstallProvider { instance_id: p.instance_id.clone() })),
            ));
        }
        if p.install.busy {
            acts = acts.push(w::small_button(
                "Cancel install",
                w::danger_style,
                Some(core(Event::CancelProviderInstall { instance_id: p.instance_id.clone() })),
            ));
        }
        if p.install.can_remove {
            acts = acts.push(w::small_button(
                "Remove runtime",
                w::ghost_style,
                Some(core(Event::RemoveProviderInstall { instance_id: p.instance_id.clone() })),
            ));
        }
        if p.config.editable {
            acts = acts.push(w::small_button(
                "Configure…",
                w::secondary_style,
                Some(Message::Overlay(Some(Overlay::ProviderInstanceEditor(Box::new(ProviderDraft::of(p)))))),
            ));
        }
        if !p.config.built_in && p.config.editable {
            acts = acts.push(w::small_button(
                "Remove",
                w::danger_style,
                Some(core(Event::RemoveProviderInstance { instance_id: p.instance_id.clone() })),
            ));
        }
        p_col = p_col.push(acts);

        // Active sign-in URL + redirect paste box
        if !p.sign_in.url.is_empty() || p.sign_in.takes_redirect {
            let mut si = Column::new().spacing(XS);
            if !p.sign_in.message.is_empty() {
                si = si.push(w::muted(&p.sign_in.message, TEXT_SMALL));
            }
            if !p.sign_in.url.is_empty() {
                si = si.push(w::small_button("Open sign-in page", w::primary_style, Some(Message::OpenUrl(p.sign_in.url.clone()))));
            }
            if p.sign_in.takes_redirect {
                let draft = app.provider_redirect_inputs.get(&p.instance_id).map(String::as_str).unwrap_or("");
                let iid = p.instance_id.clone();
                let iid2 = p.instance_id.clone();
                let val = draft.to_owned();
                si = si.push(
                    row![
                        w::input("Paste redirect URL from browser…", draft)
                            .on_input(move |u| Message::ProviderRedirectChanged { instance_id: iid.clone(), url: u })
                            .width(Length::Fill),
                        w::small_button(
                            "Complete",
                            w::primary_style,
                            (!val.trim().is_empty()).then(|| core(Event::CompleteProviderSignIn {
                                instance_id: iid2,
                                callback_url: val,
                            })),
                        ),
                    ]
                    .spacing(XS),
                );
            }
            p_col = p_col.push(container(si).padding(S).style(w::banner_style(|c| c.info)));
        }

        if p.install.busy || !p.install.message.is_empty() {
            p_col = p_col.push(w::muted(
                format!("{} {}", p.install.progress_label, p.install.message).trim().to_owned(),
                TEXT_CAPTION,
            ));
        }

        col = col.push(w::card(p_col).width(Length::Fill));
    }

    col.into()
}

fn keybindings_tab(app: &App) -> Element<'_, Message> {
    let mut col = Column::new().spacing(XS).push(w::heading("Server keybindings"));
    for kb in &app.vm.settings.keybindings {
        col = col.push(
            row![
                text(&kb.command).size(TEXT_SMALL).font(fonts::MONO).width(Length::FillPortion(3)),
                w::kbd(&kb.shortcut),
                w::faint(&kb.condition, TEXT_CAPTION).width(Length::FillPortion(2)),
                w::icon_button(
                    Icon::Trash2,
                    "Remove binding",
                    Some(core(Event::RemoveKeybinding {
                        command: kb.command.clone(),
                        shortcut: kb.shortcut.clone(),
                        condition: kb.condition.clone(),
                    })),
                ),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
    }
    w::card(col).width(Length::Fill).into()
}

fn scheduled_tab(app: &App) -> Element<'_, Message> {
    let s = &app.vm.settings;
    let mut col = Column::new().spacing(M);
    let default_proj = s.projects.first().map(|p| p.id.clone()).unwrap_or_default();
    col = col.push(
        row![
            w::heading("Scheduled Tasks"),
            w::faint(s.scheduled_time_zone.as_str(), TEXT_CAPTION),
            Space::new().width(Length::Fill),
            w::small_button(
                "New Task…",
                w::primary_style,
                (!default_proj.is_empty()).then(|| Message::Overlay(Some(Overlay::ScheduledTaskEditor(Box::new(ScheduledDraft::new_task(default_proj)))))),
            ),
        ]
        .align_y(Alignment::Center),
    );

    if s.scheduled_tasks.is_empty() {
        col = col.push(w::faint("No scheduled tasks.", TEXT_UI));
    }

    for task in &s.scheduled_tasks {
        let mut t_col = Column::new().spacing(XS);
        let mut hdr = row![
            text(&task.name).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
            w::small_button(
                if task.enabled { "Enabled" } else { "Paused" },
                if task.enabled { w::primary_style } else { w::secondary_style },
                Some(core(Event::SetScheduledEnabled { id: task.id.clone(), enabled: !task.enabled })),
            ),
        ]
        .spacing(XS)
        .align_y(Alignment::Center);

        if task.schedule_kind != "webhook" {
            hdr = hdr.push(w::small_button("Run Now", w::secondary_style, Some(core(Event::RunScheduledNow { id: task.id.clone() }))));
        }
        // The task's own thread, else the one its last run started.
        let thread = if task.thread_id.is_empty() { &task.last_thread_id } else { &task.thread_id };
        if !thread.is_empty() {
            hdr = hdr.push(w::small_button("Open Thread", w::ghost_style, Some(core(Event::SelectThread { thread_id: thread.clone() }))));
        }
        let open_runs = s.scheduled_runs_task == task.id;
        hdr = hdr
            .push(w::small_button(
                if open_runs { "Hide Runs" } else { "Runs" },
                w::ghost_style,
                Some(core(Event::LoadScheduledRuns { id: if open_runs { String::new() } else { task.id.clone() } })),
            ))
            .push(w::small_button(
                "Edit",
                w::secondary_style,
                Some(Message::Overlay(Some(Overlay::ScheduledTaskEditor(Box::new(ScheduledDraft::from_task(task)))))),
            ))
            .push(w::icon_button(Icon::Trash2, "Delete task", Some(core(Event::DeleteScheduled { id: task.id.clone() }))));

        t_col = t_col
            .push(hdr)
            .push(w::muted(&task.prompt, TEXT_SMALL))
            .push(w::faint(
                format!("{} · {} · {} · next {} · last {}", task.schedule, task.project_title, destination(task, &s.scheduled_thread_choices), task.next_run, task.last_run),
                TEXT_CAPTION,
            ));

        if !task.run_state.is_empty() {
            t_col = t_col.push(w::colored(&task.run_state, TEXT_CAPTION, |c| c.accent));
        } else if !task.last_error.is_empty() {
            t_col = t_col.push(w::colored(&task.last_error, TEXT_CAPTION, |c| c.danger));
        }

        if task.schedule_kind == "webhook" && !task.webhook_url.is_empty() {
            t_col = t_col.push(
                row![
                    text(&task.webhook_url).size(TEXT_CAPTION).font(fonts::MONO).width(Length::Fill),
                    w::small_button("Copy URL", w::ghost_style, Some(Message::CopyText(task.webhook_url.clone()))),
                    w::small_button("Rotate Token", w::secondary_style, Some(core(Event::RotateWebhookToken { id: task.id.clone() }))),
                    w::small_button(
                        if s.webhook_deliveries_task == task.id { "Refresh Deliveries" } else { "Deliveries" },
                        w::ghost_style,
                        Some(core(Event::LoadWebhookDeliveries { id: task.id.clone() })),
                    ),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            );
            if s.webhook_deliveries_task == task.id {
                t_col = t_col.push(webhook_deliveries(&s.webhook_deliveries));
            }
        }

        if open_runs {
            for run in &s.scheduled_runs {
                t_col = t_col.push(
                    row![
                        w::colored(&run.status, TEXT_CAPTION, if run.failed { |c| c.danger } else { |c| c.success }),
                        w::muted(format!("{} · {} · {}", run.trigger, run.started, run.thread_title), TEXT_CAPTION).width(Length::Fill),
                        if !run.thread_id.is_empty() {
                            w::small_button(
                                "Open",
                                w::ghost_style,
                                Some(core(Event::SelectThread { thread_id: run.thread_id.clone() })),
                            )
                            .into()
                        } else {
                            Element::from(Space::new())
                        },
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                );
            }
        }

        col = col.push(w::card(t_col).width(Length::Fill));
    }

    col.into()
}

fn archived_tab(app: &App) -> Element<'_, Message> {
    let mut col = Column::new().spacing(XS).push(w::heading("Archived Threads"));
    if app.vm.settings.archived.is_empty() {
        col = col.push(w::faint("No archived threads.", TEXT_UI));
    }
    for t in &app.vm.settings.archived {
        col = col.push(
            w::card(
                row![
                    column![
                        text(&t.title).size(TEXT_UI).font(fonts::MEDIUM),
                        w::muted(format!("{} · {}", t.project_title, t.age), TEXT_CAPTION),
                    ]
                    .width(Length::Fill),
                    w::small_button("Unarchive", w::secondary_style, Some(core(Event::ArchiveThread { thread_id: t.id.clone(), archived: false }))),
                    w::icon_button(
                        Icon::Trash2,
                        "Delete permanently",
                        Some(Message::Overlay(Some(Overlay::ConfirmDeleteThread {
                            thread_id: t.id.clone(),
                            title: t.title.clone(),
                        }))),
                    ),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill),
        );
    }
    col.into()
}

fn projects_tab(app: &App) -> Element<'_, Message> {
    let files = &app.vm.settings.project_icon_files;
    let mut col = Column::new().spacing(S).push(w::heading("Projects"));
    for p in &app.vm.settings.projects {
        let picking = files.project_id == p.id;
        let mut title = row![text(&p.title).size(TEXT_UI).font(fonts::SEMIBOLD)].spacing(XS).align_y(Alignment::Center);
        if p.icon.saving {
            title = title.push(w::badge_capsule("Saving icon…", Some(|c| c.info)));
        }
        let mut info = column![title, w::faint(format!("{} · {} threads", p.workspace_root, p.thread_count), TEXT_CAPTION)].width(Length::Fill);
        if let Some(path) = &p.icon.file_path {
            info = info.push(w::faint(format!("Icon from {path}"), TEXT_CAPTION).font(fonts::MONO));
        }
        let mut card_col = Column::new().spacing(S).push(
            row![
                w::project_icon_tile(&p.icon, 28.0, &app.images),
                info,
                w::small_button(
                    "Icon…",
                    w::secondary_style,
                    Some(Message::Overlay(Some(Overlay::ProjectIconPicker {
                        project_id: p.id.clone(),
                        kind: p.icon.kind.clone(),
                        text: p.icon.text.clone(),
                        symbol: p.icon.symbol.clone(),
                        color: p.icon.color.clone(),
                        file_query: String::new(),
                    }))),
                ),
                w::small_button(
                    if picking { "Close Files" } else { "From File…" },
                    if picking { w::primary_style } else { w::secondary_style },
                    Some(core(if picking {
                        Event::CloseProjectIconFiles { project_id: p.id.clone() }
                    } else {
                        Event::SearchProjectIconFiles { project_id: p.id.clone(), query: icon_query(app, &p.id).to_owned() }
                    })),
                ),
                w::small_button(
                    "Rename…",
                    w::ghost_style,
                    Some(Message::Overlay(Some(Overlay::RenameProject {
                        project_id: p.id.clone(),
                        value: p.title.clone(),
                    }))),
                ),
                if !p.is_scratch {
                    w::icon_button(
                        Icon::Trash2,
                        "Remove project",
                        Some(Message::Overlay(Some(Overlay::ConfirmRemoveProject {
                            project_id: p.id.clone(),
                            title: p.title.clone(),
                        }))),
                    )
                } else {
                    Space::new().into()
                },
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
        // A chosen icon can go back to the one Domovoi finds or makes up.
        if p.icon.custom || p.icon.file_path.is_some() {
            let color = if PROJECT_KEYS.contains(&p.automatic_icon.color.as_str()) { p.automatic_icon.color.clone() } else { "gray".into() };
            card_col = card_col.push(
                row![
                    w::project_icon_tile(&p.automatic_icon, 20.0, &app.images),
                    w::muted("Automatic icon: a project icon file when found, or initials from the name.", TEXT_CAPTION).width(Length::Fill),
                    w::small_button(
                        "Use Automatic",
                        w::ghost_style,
                        (!p.icon.saving).then(|| core(Event::SetProjectIcon {
                            project_id: p.id.clone(),
                            kind: String::new(),
                            text: String::new(),
                            symbol: String::new(),
                            color,
                        })),
                    ),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            );
        }
        if picking {
            card_col = card_col.push(icon_file_picker(app, p));
        }
        col = col.push(w::card(card_col).width(Length::Fill));
    }
    col.into()
}

/// The search typed into a project's icon file picker.
fn icon_query<'a>(app: &'a App, project_id: &str) -> &'a str {
    app.setting_text_drafts.get(&icon_query_key(project_id)).map_or("", String::as_str)
}

fn icon_query_key(project_id: &str) -> String {
    format!("project-icon-files:{project_id}")
}

/// Images in the project's files to use as its icon. A file's preview is
/// loaded on request; one with a preview can be chosen.
fn icon_file_picker<'a>(app: &'a App, p: &'a ProjectSettingsView) -> Element<'a, Message> {
    let files = &app.vm.settings.project_icon_files;
    let query = icon_query(app, &p.id);
    let search = core(Event::SearchProjectIconFiles { project_id: p.id.clone(), query: query.to_owned() });
    let key = icon_query_key(&p.id);
    let mut col = Column::new().spacing(XS).push(
        row![
            w::input("Search project images…", query)
                .on_input(move |value| Message::SettingTextChanged { key: key.clone(), value })
                .on_submit(search.clone())
                .width(Length::Fill),
            w::small_button("Search", w::secondary_style, Some(search)),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    );
    if files.loading {
        return col.push(w::muted("Finding project images…", TEXT_SMALL)).into();
    }
    if let Some(err) = &files.error {
        return col.push(w::colored(err, TEXT_SMALL, |c| c.danger)).into();
    }
    if files.files.is_empty() {
        return col.push(w::faint(if query.is_empty() { "No project images found." } else { "No matching project images." }, TEXT_SMALL)).into();
    }
    for f in &files.files {
        let current = p.icon.file_path.as_deref() == Some(f.path.as_str());
        let thumb: Element<'a, Message> = match f.url.as_deref().and_then(|u| app.images.get(u)) {
            Some(handle) => container(iced::widget::image(handle.clone()).width(24.0).height(24.0)).width(24.0).height(24.0).clip(true).into(),
            None => container(w::themed_icon(if f.unavailable { Icon::TriangleAlert } else { Icon::Image }, ICON_SMALL, |c| c.muted)).center_x(24.0).center_y(24.0).into(),
        };
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        let action: Element<'a, Message> = if current {
            w::badge_capsule("Current", Some(|c| c.success))
        } else if f.url.is_some() {
            w::small_button("Use", w::primary_style, (!p.icon.saving).then(|| core(Event::SetProjectIconFile { project_id: p.id.clone(), path: f.path.clone() }))).into()
        } else if f.unavailable {
            w::faint("Preview unavailable", TEXT_CAPTION).into()
        } else if f.refresh {
            w::small_button("Preview", w::secondary_style, Some(core(Event::LoadProjectIconThumbnail { project_id: p.id.clone(), path: f.path.clone() }))).into()
        } else {
            w::faint("Loading preview…", TEXT_CAPTION).into()
        };
        col = col.push(
            row![
                thumb,
                column![text(name).size(TEXT_SMALL), w::faint(&f.path, TEXT_CAPTION).font(fonts::MONO)].width(Length::Fill),
                action,
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
    }
    if files.truncated {
        col = col.push(w::faint("Search to narrow the results.", TEXT_CAPTION));
    }
    container(col).padding(M).width(Length::Fill).style(w::inset_sunken).into()
}

fn maintenance_tab(app: &App) -> Element<'_, Message> {
    let st = &app.vm.settings.storage;
    let diag = &app.vm.settings.diagnostics;
    let mut proc_col = Column::new().spacing(XS).push(Space::new().height(XS)).push(w::heading("Server processes"));
    for pr in &diag.processes {
        proc_col = proc_col.push(
            row![
                text(format!("PID {}", pr.pid)).size(TEXT_CAPTION).font(fonts::MONO).width(Length::Fixed(80.0)),
                text(&pr.command).size(TEXT_CAPTION).font(fonts::MONO).width(Length::Fill),
                w::muted(&pr.cpu, TEXT_CAPTION),
                w::muted(&pr.memory, TEXT_CAPTION),
            ]
            .spacing(S),
        );
    }
    column![
        w::card(
            column![
                w::heading("Storage"),
                kv_row("Database size", &st.database_size),
                kv_row("Reclaimable", &st.reclaimable),
                kv_row("Last cleanup", &st.last_cleanup),
                Space::new().height(XS),
                row![
                    w::small_button("Run Cleanup", w::secondary_style, (!st.busy).then(|| core(Event::RunStorageCleanup))),
                    w::small_button("Reclaim Space", w::secondary_style, (!st.busy).then(|| core(Event::ReclaimDatabase))),
                ]
                .spacing(S),
            ]
            .spacing(S),
        )
        .width(Length::Fill),
        w::card(
            column![
                w::heading("Diagnostics"),
                kv_row("CPU", &diag.cpu),
                kv_row("Memory", &diag.memory),
                proc_col,
            ]
            .spacing(S),
        )
        .width(Length::Fill),
    ]
    .spacing(L)
    .into()
}

fn access_tab(app: &App) -> Element<'_, Message> {
    let rem = &app.vm.settings.remote;
    let mut col = Column::new().spacing(L);

    if rem.available {
        let mut r_col = Column::new().spacing(S).push(
            row![
                column![
                    text("Remote Access (iroh)").size(TEXT_UI).font(fonts::SEMIBOLD),
                    w::muted("Reach this server from anywhere over encrypted QUIC without port forwarding.", TEXT_SMALL),
                ]
                .width(Length::Fill),
                w::small_button(
                    if rem.enabled { "Disable" } else { "Enable" },
                    if rem.enabled { w::danger_style } else { w::primary_style },
                    (!rem.busy).then(|| core(Event::SetRemoteAccess { enabled: !rem.enabled })),
                ),
            ]
            .spacing(M)
            .align_y(Alignment::Center),
        );
        if !rem.endpoint_id.is_empty() {
            r_col = r_col.push(kv_row("Endpoint ID", &rem.endpoint_id));
        }
        if !rem.state.is_empty() {
            r_col = r_col.push(kv_row("State", &rem.state));
        }
        if rem.running {
            r_col = r_col.push(kv_row("Relay", if rem.home_relay.is_empty() { rem.relays.first().map_or("connecting…", String::as_str) } else { rem.home_relay.as_str() }));
        }
        if let Some(err) = &rem.error {
            r_col = r_col.push(w::colored(err, TEXT_SMALL, |c| c.danger));
        }
        if rem.enabled {
            r_col = r_col.push(Space::new().height(XS)).push(w::icon_label_button(
                Icon::QrCode,
                "Create Pairing Link & QR",
                w::Kind::Primary,
                (!rem.busy).then(|| core(Event::CreatePairingLink)),
            ));
        }
        if !rem.link.is_empty() {
            let mut qr_row = row![].spacing(L).align_y(Alignment::Center);
            if let Some((_, data)) = &app.pairing_qr {
                // Dark-on-light QR code on a white card with quiet zone (per design system).
                qr_row = qr_row.push(
                    container(qr_code(data).cell_size(4).style(|_| qr_code::Style {
                        cell: Color::BLACK,
                        background: Color::WHITE,
                    }))
                    .padding(M)
                    .style(|_| container::Style {
                        background: Some(Background::Color(Color::WHITE)),
                        border: Border { radius: RADIUS_CHIP.into(), ..Border::default() },
                        ..Default::default()
                    }),
                );
            }
            qr_row = qr_row.push(
                column![
                    text("Scan with Domovoi on iOS/Android or copy link for Desktop").size(TEXT_SMALL).font(fonts::MEDIUM),
                    kv_row("Code", &rem.code),
                    kv_row("Expires", &rem.link_expires_at),
                    row![
                        w::small_button("Copy Link", w::primary_style, Some(Message::CopyText(rem.link.clone()))),
                        w::small_button("Copy Code", w::secondary_style, Some(Message::CopyText(rem.code.clone()))),
                    ]
                    .spacing(XS),
                ]
                .spacing(XS),
            );
            r_col = r_col.push(qr_row);
        }
        col = col.push(w::card(r_col).width(Length::Fill));
    }

    let mut acc_col = Column::new().spacing(XS).push(w::heading("Paired Devices & Sessions"));
    for entry in &app.vm.settings.access {
        let ic = match entry.device_type.as_str() {
            "mobile" | "tablet" => Icon::Smartphone,
            _ => Icon::Laptop,
        };
        acc_col = acc_col.push(
            row![
                w::themed_icon(ic, ICON, |c| c.muted),
                column![
                    row![
                        text(&entry.label).size(TEXT_UI).font(fonts::MEDIUM),
                        if entry.current { w::badge_capsule("This device", Some(|c| c.accent)) } else { Space::new().into() },
                        if entry.connected { w::badge_capsule("Online", Some(|c| c.success)) } else { Space::new().into() },
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                    w::faint(&entry.detail, TEXT_CAPTION),
                ]
                .width(Length::Fill),
                if !entry.current {
                    w::small_button("Revoke", w::danger_style, Some(core(Event::RevokeClient { session_id: entry.id.clone() })))
                        .into()
                } else {
                    Element::from(Space::new())
                },
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(w::card(acc_col).width(Length::Fill));
    col.into()
}

fn companion_tab(app: &App) -> Element<'_, Message> {
    let target_env = active_server(app);

    let (env_id, look) = match target_env {
        Some(e) => (e.id.clone(), e.companion.look.clone()),
        None => (String::new(), app.vm.connection.companion.look.clone()),
    };

    let mut col = Column::new().spacing(L);

    let preview = container(companion::view(look.clone(), companion::Mood::Idle, 96.0, app.tick_phase, app.companion_poked))
        .center_x(Length::Fill)
        .padding(M);
    col = col.push(w::card(preview).width(Length::Fill));

    // Body color swatches
    let mut swatches = row![].spacing(XS);
    for &key in PROJECT_KEYS.iter().take(9) {
        let mut next = look.clone();
        next.body = key.to_owned();
        let active = look.body == key;
        let eid = env_id.clone();
        swatches = swatches.push(
            button(Space::new().width(20.0).height(20.0))
                .padding(2.0)
                .style(move |t: &Theme, _| button::Style {
                    background: Some(Background::Color(project_color(key, colors(t).dark))),
                    border: Border {
                        radius: RADIUS_PILL.into(),
                        width: if active { 2.0 } else { 1.0 },
                        color: if active { colors(t).text } else { colors(t).border },
                    },
                    ..Default::default()
                })
                .on_press(Message::CompanionLookChanged { env_id: eid, look: next }),
        );
    }
    let mut swatches2 = row![].spacing(XS);
    for &key in PROJECT_KEYS.iter().skip(9) {
        let mut next = look.clone();
        next.body = key.to_owned();
        let active = look.body == key;
        let eid = env_id.clone();
        swatches2 = swatches2.push(
            button(Space::new().width(20.0).height(20.0))
                .padding(2.0)
                .style(move |t: &Theme, _| button::Style {
                    background: Some(Background::Color(project_color(key, colors(t).dark))),
                    border: Border {
                        radius: RADIUS_PILL.into(),
                        width: if active { 2.0 } else { 1.0 },
                        color: if active { colors(t).text } else { colors(t).border },
                    },
                    ..Default::default()
                })
                .on_press(Message::CompanionLookChanged { env_id: eid, look: next }),
        );
    }

    let eye_row = choice_row("Eye shape", &["round", "oval", "pill", "sleepy"], &look.eye_shape, |v| {
        let mut next = look.clone();
        next.eye_shape = v.to_owned();
        Message::CompanionLookChanged { env_id: env_id.clone(), look: next }
    });
    let ears_row = choice_row("Ears", &["none", "round", "pointy"], &look.ears, |v| {
        let mut next = look.clone();
        next.ears = v.to_owned();
        Message::CompanionLookChanged { env_id: env_id.clone(), look: next }
    });
    let pupil_sz_row = choice_row("Pupil size", &["small", "medium", "large"], &look.pupil_size, |v| {
        let mut next = look.clone();
        next.pupil_size = v.to_owned();
        Message::CompanionLookChanged { env_id: env_id.clone(), look: next }
    });

    let mut toggles = row![].spacing(S);
    {
        let mut n = look.clone();
        n.antenna = !n.antenna;
        toggles = toggles.push(w::small_button(
            if look.antenna { "Antenna: On" } else { "Antenna: Off" },
            if look.antenna { w::primary_style } else { w::secondary_style },
            Some(Message::CompanionLookChanged { env_id: env_id.clone(), look: n }),
        ));
    }
    {
        let mut n = look.clone();
        n.eyebrows = !n.eyebrows;
        toggles = toggles.push(w::small_button(
            if look.eyebrows { "Eyebrows: On" } else { "Eyebrows: Off" },
            if look.eyebrows { w::primary_style } else { w::secondary_style },
            Some(Message::CompanionLookChanged { env_id: env_id.clone(), look: n }),
        ));
    }
    {
        let mut n = look.clone();
        n.follow = !n.follow;
        toggles = toggles.push(w::small_button(
            if look.follow { "Follow cursor: On" } else { "Follow cursor: Off" },
            if look.follow { w::primary_style } else { w::secondary_style },
            Some(Message::CompanionLookChanged { env_id: env_id.clone(), look: n }),
        ));
    }
    if !env_id.is_empty() {
        toggles = toggles.push(w::small_button("Reset", w::ghost_style, Some(core(Event::ResetCompanion { id: env_id }))));
    }

    col = col.push(
        w::card(
            column![
                w::heading("Body color"),
                swatches,
                swatches2,
                eye_row,
                ears_row,
                pupil_sz_row,
                toggles,
            ]
            .spacing(S),
        )
        .width(Length::Fill),
    );
    col.into()
}

fn choice_row<'a>(label: &'a str, opts: &'a [&'a str], current: &str, mk: impl Fn(&'a str) -> Message) -> Element<'a, Message> {
    let mut r = row![w::muted(label, TEXT_SMALL).width(Length::Fixed(90.0))].spacing(XS).align_y(Alignment::Center);
    for &o in opts {
        r = r.push(w::small_button(o, if o == current { w::primary_style } else { w::secondary_style }, Some(mk(o))));
    }
    r.into()
}

fn kv_row<'a>(k: &'a str, v: impl Into<String>) -> Element<'a, Message> {
    let v: String = v.into();
    row![
        w::muted(k, TEXT_SMALL).width(Length::Fixed(120.0)),
        text(if v.is_empty() { "—".to_owned() } else { v }).size(TEXT_SMALL).font(fonts::MONO),
    ]
    .spacing(S)
    .into()
}

/// Where a scheduled task's runs go, as its row says it.
fn destination(task: &ScheduledTaskView, choices: &[ScheduledThreadChoiceView]) -> String {
    if task.thread_mode == "new_thread" {
        return "new thread each run".into();
    }
    if task.thread_id.is_empty() {
        return "the thread its first run starts".into();
    }
    let title = if task.thread_title.is_empty() { "a thread not on this device".to_owned() } else { format!("“{}”", task.thread_title) };
    let mut out = if task.thread_mode == "fresh_session" { format!("in {title}, fresh session") } else { format!("in {title}") };
    // A bound task's runs follow the thread's model.
    if let Some(choice) = choices.iter().find(|c| c.id == task.thread_id)
        && !choice.model_label.is_empty() {
            out = format!("{out} ({})", choice.model_label);
        }
    out
}

/// A webhook task's recent requests: what became of each, and when.
fn webhook_deliveries(deliveries: &[WebhookDeliveryView]) -> Element<'_, Message> {
    let mut col = Column::new().spacing(XXS);
    if deliveries.is_empty() {
        col = col.push(w::faint("No requests yet.", TEXT_CAPTION));
    }
    for d in deliveries {
        let tone: w::Tone = match d.outcome.as_str() {
            "accepted" => |c| c.success,
            "dispatch failed" | "rejected signature" => |c| c.danger,
            _ => |c| c.muted,
        };
        col = col.push(
            row![
                w::colored(&d.outcome, TEXT_CAPTION, tone).font(fonts::MEDIUM),
                w::muted(&d.method, TEXT_CAPTION).font(fonts::MONO),
                w::muted(&d.detail, TEXT_CAPTION).width(Length::Fill),
                w::faint(&d.received, TEXT_CAPTION),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
    }
    container(col).padding(M).width(Length::Fill).style(w::inset_sunken).into()
}
