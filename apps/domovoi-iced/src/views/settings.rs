//! Settings modal covering desktop Appearance and every server settings section:
//! General, Providers (sign-in, install, instance editor), Keybindings,
//! Scheduled Tasks (runs, webhooks, editor), Archived, Projects (icon picker),
//! Maintenance (Storage + Diagnostics), Access (sessions + iroh Remote Access QR),
//! and Companion mascot customizer.

use domovoi_core::Event;
use domovoi_core::view::{ProjectSettingsView, ScheduledTaskView, ScheduledThreadChoiceView, WebhookDeliveryView};
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

const SECTIONS: [(SettingsSection, Icon, &str, bool); 10] = [
    (SettingsSection::Appearance, Icon::Sun, "Appearance", false),
    (SettingsSection::General, Icon::Settings, "General", true),
    (SettingsSection::Providers, Icon::Cpu, "Providers", true),
    (SettingsSection::Keybindings, Icon::Keyboard, "Keybindings", true),
    (SettingsSection::Scheduled, Icon::CalendarClock, "Scheduled", true),
    (SettingsSection::Archived, Icon::Archive, "Archived", true),
    (SettingsSection::Projects, Icon::Folder, "Projects", true),
    (SettingsSection::Maintenance, Icon::Wrench, "Maintenance", true),
    (SettingsSection::Access, Icon::Key, "Access & Remote", true),
    (SettingsSection::Companion, Icon::Smile, "Companion", false),
];

pub fn view(app: &App, active: SettingsSection) -> Element<'_, Message> {
    let connected = matches!(app.vm.connection.state.as_str(), "connected" | "reconnecting");

    let mut nav = Column::new().spacing(XXS).padding(S).width(Length::Fixed(184.0));
    for (sec, ic, label, needs_conn) in SECTIONS {
        if needs_conn && !connected {
            continue;
        }
        let selected = active == sec;
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

    let content: Element<'_, Message> = match active {
        SettingsSection::Appearance => appearance_tab(app),
        SettingsSection::General => general_tab(app),
        SettingsSection::Providers => providers_tab(app),
        SettingsSection::Keybindings => keybindings_tab(app),
        SettingsSection::Scheduled => scheduled_tab(app),
        SettingsSection::Archived => archived_tab(app),
        SettingsSection::Projects => projects_tab(app),
        SettingsSection::Maintenance => maintenance_tab(app),
        SettingsSection::Access => access_tab(app),
        SettingsSection::Companion => companion_tab(app),
    };

    container(
        column![
            container(w::dialog_header(Some((Icon::Settings, |c| c.accent)), "Settings", Some(Message::Overlay(None))))
                .padding([M, L]),
            w::divider(),
            row![
                container(nav).height(Length::Fill).style(w::surface),
                w::vdivider(),
                container(w::scroll(container(content).padding(L)).height(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(w::page),
            ]
            .height(Length::Fill),
        ],
    )
    .width(880)
    .height(640)
    .style(w::popover_style)
    .into()
}

fn appearance_tab(app: &App) -> Element<'_, Message> {
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
        Space::new().height(M),
        w::heading("Transcript text size"),
        row![
            slider(TEXT_MIN..=TEXT_MAX, app.prefs.text_size, Message::TextSize).step(1.0_f32).width(Length::Fixed(240.0)),
            text(format!("{} px", app.prefs.text_size.round())).size(TEXT_UI).font(fonts::MONO),
        ]
        .spacing(M)
        .align_y(Alignment::Center),
        w::muted("Uses Domovoi's Hearth palette with bundled Inter and JetBrains Mono fonts.", TEXT_SMALL),
    ]
    .spacing(S)
    .into()
}

fn general_tab(app: &App) -> Element<'_, Message> {
    let conn = &app.vm.connection;
    let mut col = Column::new().spacing(M).push(
        w::card(
            column![
                w::heading("Connected Server"),
                kv_row("Address", &conn.server_url),
                kv_row("Environment", &conn.environment_label),
                kv_row("Version", &conn.server_version),
                kv_row("Platform", &conn.platform),
                row![
                    w::small_button("Disconnect", w::danger_style, Some(core(Event::Disconnect))),
                    w::small_button("Resync", w::secondary_style, Some(core(Event::Resync))),
                ]
                .spacing(S),
            ]
            .spacing(XS),
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
            g_col = g_col.push(
                row![
                    column![
                        text(&field.label).size(TEXT_UI).font(fonts::MEDIUM),
                        if !field.help.is_empty() { w::muted(&field.help, TEXT_CAPTION) } else { w::muted("", TEXT_CAPTION) },
                    ]
                    .width(Length::Fill),
                    control,
                ]
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
    container(col).padding(S).width(Length::Fill).style(w::sunken).into()
}

fn maintenance_tab(app: &App) -> Element<'_, Message> {
    let st = &app.vm.settings.storage;
    let diag = &app.vm.settings.diagnostics;
    let mut proc_col = Column::new().spacing(XXS).push(w::heading("Server processes"));
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
                row![
                    w::small_button("Run Cleanup", w::secondary_style, (!st.busy).then(|| core(Event::RunStorageCleanup))),
                    w::small_button("Reclaim Space", w::secondary_style, (!st.busy).then(|| core(Event::ReclaimDatabase))),
                ]
                .spacing(S),
            ]
            .spacing(XS),
        )
        .width(Length::Fill),
        w::card(
            column![
                w::heading("Diagnostics"),
                kv_row("CPU", &diag.cpu),
                kv_row("Memory", &diag.memory),
                proc_col,
            ]
            .spacing(XS),
        )
        .width(Length::Fill),
    ]
    .spacing(M)
    .into()
}

fn access_tab(app: &App) -> Element<'_, Message> {
    let rem = &app.vm.settings.remote;
    let mut col = Column::new().spacing(M);

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
            r_col = r_col.push(w::icon_label_button(
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
    let envs = &app.vm.connection.environments;
    let target_env = app
        .companion_server_id
        .as_deref()
        .and_then(|id| envs.iter().find(|e| e.id == id))
        .or_else(|| envs.iter().find(|e| e.active))
        .or_else(|| envs.first());

    let (env_id, look) = match target_env {
        Some(e) => (e.id.clone(), e.companion.look.clone()),
        None => (String::new(), app.vm.connection.companion.look.clone()),
    };

    let mut col = Column::new().spacing(M);
    if !envs.is_empty() {
        let mut srv_row = row![].spacing(XS);
        for e in envs {
            srv_row = srv_row.push(
                button(text(&e.label).size(TEXT_SMALL))
                    .padding([4.0, S])
                    .style(w::chip_style(e.id == env_id))
                    .on_press(Message::CompanionServerSelected(e.id.clone())),
            );
        }
        col = col.push(srv_row);
    }

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
    container(col).padding(S).width(Length::Fill).style(w::sunken).into()
}
