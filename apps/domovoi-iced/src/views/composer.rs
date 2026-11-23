//! Floating thread composer: multi-line text editor, @mentions, /commands,
//! $skills, attachments, model/effort/permissions/traits popovers, draft stash,
//! and Send / Steer / Stop / Force Stop actions.

use domovoi_core::Event;
use iced::widget::{Column, Space, button, column, container, row, text, text_editor};
use iced::{Alignment, Background, Border, Element, Length, Padding, Theme};

use crate::app::{App, ComposerPopover, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

/// Rows shown in the mention / command lists (and cycled by Up/Down).
pub const LIST_LIMIT: usize = 12;

/// What the composer's keys do beyond editing, decided from the state at render.
struct KeyContext {
    /// `@file` suggestions are open (the core tracks the highlight).
    mention: Option<String>,
    /// A `/command` or `$skill` list is open (the shell tracks the highlight).
    commands: bool,
    /// Up on the first line / Down on the last recalls earlier messages.
    recall: bool,
    first_line: bool,
    last_line: bool,
    toggles_plan: bool,
}

fn composer_key(cx: &KeyContext, kp: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::{Key, key::Named};
    use text_editor::Binding;
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let m = kp.modifiers;
    let plain = !m.shift() && !m.alt() && !m.control() && !m.logo();
    let list_open = cx.mention.is_some() || cx.commands;
    let custom = |msg: Message| Some(Binding::Custom(msg));
    match kp.key.as_ref() {
        Key::Named(Named::Enter) if plain => return custom(Message::ComposerEnter),
        Key::Named(Named::Tab) if plain && list_open => return custom(Message::ComposerEnter),
        Key::Named(Named::Tab) if m.shift() && !list_open && cx.toggles_plan => return custom(Message::Shortcut("toggle-plan")),
        Key::Named(dir @ (Named::ArrowUp | Named::ArrowDown)) if plain => {
            let delta = if dir == Named::ArrowUp { -1 } else { 1 };
            if cx.mention.is_some() {
                return custom(core(Event::MoveMention { delta }));
            }
            if cx.commands {
                return custom(Message::MoveCommand(delta));
            }
            if cx.recall && ((delta < 0 && cx.first_line) || (delta > 0 && cx.last_line)) {
                return custom(Message::RecallPrompt(delta));
            }
        }
        _ => {}
    }
    Binding::from_key_press(kp)
}

pub fn view(app: &App) -> Element<'_, Message> {
    let comp = &app.vm.composer;
    let planning = comp.interaction_mode == "plan";

    let mut stack_col = Column::new().spacing(S).max_width(TRANSCRIPT_MAX_WIDTH).width(Length::Fill);

    // Popover above composer: mentions, skills, slash commands, or option pickers.
    if let Some(m) = comp.mention.as_ref().filter(|m| !m.suggestions.is_empty()) {
        stack_col = stack_col.push(mentions_popover(m));
    } else if !comp.skills.is_empty() {
        stack_col = stack_col.push(commands_popover(&comp.skills, "$", app.command_highlight, |name| core(Event::PickSkill { name })));
    } else if !comp.slash_commands.is_empty() {
        stack_col = stack_col.push(commands_popover(&comp.slash_commands, "/", app.command_highlight, |name| core(Event::PickSlashCommand { name })));
    } else if let Some(pop) = app.composer_popover {
        stack_col = stack_col.push(option_popover(app, pop));
    }

    // Main composer card
    let mut panel_col = Column::new().spacing(S);

    // Attachments strip
    if !comp.attachments.is_empty() {
        let mut att_row = row![].spacing(XS).align_y(Alignment::Center);
        for att in &comp.attachments {
            att_row = att_row.push(
                container(
                    row![
                        w::themed_icon(if att.uploading { Icon::Loader } else { Icon::Image }, ICON_XS, |c| c.accent),
                        text(&att.name).size(TEXT_SMALL),
                        w::faint(if att.uploading { "Uploading…" } else { att.size_label.as_str() }, TEXT_CAPTION),
                        button(w::themed_icon(Icon::X, ICON_XS, |c| c.muted))
                            .padding(2.0)
                            .style(w::ghost_style)
                            .on_press(core(Event::RemoveAttachment { id: att.id.clone() })),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                )
                .padding([3.0, S])
                .style(w::surface),
            );
        }
        panel_col = panel_col.push(att_row);
    }

    // Blocked / offline / hint banners
    if !comp.blocked_reason.is_empty() {
        panel_col = panel_col.push(
            container(
                row![
                    w::themed_icon(Icon::TriangleAlert, ICON_SMALL, |c| c.warning),
                    w::colored(&comp.blocked_reason, TEXT_SMALL, |c| c.warning),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            )
            .padding([XS, S])
            .width(Length::Fill)
            .style(w::banner_style(|c| c.warning)),
        );
    }

    // Multi-line text editor
    let placeholder = if comp.placeholder.is_empty() {
        "Message the agent… (Enter to send, Shift+Enter for newline)"
    } else {
        comp.placeholder.as_str()
    };
    let cursor = app.draft.cursor();
    let key_cx = KeyContext {
        mention: comp.mention.as_ref().filter(|m| !m.suggestions.is_empty()).map(|m| m.query.clone()),
        commands: !comp.skills.is_empty() || !comp.slash_commands.is_empty(),
        recall: app.vm.thread.is_some() && cursor.selection.is_none(),
        first_line: cursor.position.line == 0,
        last_line: cursor.position.line + 1 >= app.draft.line_count(),
        toggles_plan: comp.shows_interaction_mode || planning,
    };
    let editor = text_editor(&app.draft)
        .placeholder(placeholder)
        .on_action(Message::DraftAction)
        .key_binding(move |kp| composer_key(&key_cx, kp))
        .size(app.prefs.text_size)
        .padding([S, S])
        .min_height(56.0)
        .max_height(200.0)
        .style(w::editor_style);
    panel_col = panel_col.push(editor);

    if let Some(err) = &app.attachment_error {
        panel_col = panel_col.push(
            row![
                w::colored(err.clone(), TEXT_SMALL, |c| c.danger).width(Length::Fill),
                w::small_button("Dismiss", w::ghost_style, Some(Message::ClearAttachmentError)),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        );
    }

    // Bottom controls bar
    let model_label = comp
        .selected_model
        .as_ref()
        .map(|m| m.label.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Choose model");
    let mut controls = row![
        w::icon_button(Icon::Paperclip, "Attach image", Some(Message::PickAttachment)),
        button(
            row![
                w::themed_icon(Icon::Cpu, ICON_SMALL, |c| c.accent),
                text(model_label).size(TEXT_SMALL).font(fonts::MEDIUM),
                w::themed_icon(Icon::ChevronDown, ICON_XS, |c| c.muted),
            ]
            .spacing(XS)
            .align_y(Alignment::Center)
        )
        .padding([4.0, S])
        .style(w::chip_style(app.composer_popover == Some(ComposerPopover::Model)))
        .on_press(Message::ComposerPopover(Some(ComposerPopover::Model))),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    if !comp.efforts.is_empty() {
        let eff_label = comp
            .selected_effort
            .as_deref()
            .and_then(|v| comp.efforts.iter().find(|e| e.value == v))
            .map(|e| e.label.as_str())
            .unwrap_or(&comp.effort_label);
        controls = controls.push(
            button(text(eff_label).size(TEXT_SMALL).font(fonts::MEDIUM))
                .padding([4.0, S])
                .style(w::chip_style(app.composer_popover == Some(ComposerPopover::Effort)))
                .on_press(Message::ComposerPopover(Some(ComposerPopover::Effort))),
        );
    }

    if comp.shows_interaction_mode {
        controls = controls.push(
            button(
                row![
                    w::themed_icon(Icon::ClipboardList, ICON_XS, move |c| if planning { c.violet } else { c.muted }),
                    text("Plan").size(TEXT_SMALL).font(fonts::MEDIUM),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            )
            .padding([4.0, S])
            .style(w::chip_style(planning))
            .on_press(core(Event::SetInteractionMode {
                mode: if planning { "default" } else { "plan" }.into(),
            })),
        );
    }

    if !comp.runtime_modes.is_empty() {
        let perm_label = comp
            .runtime_modes
            .iter()
            .find(|m| m.value == comp.runtime_mode)
            .map(|m| m.label.as_str())
            .unwrap_or(&comp.runtime_mode);
        let is_full = comp.runtime_mode == "full-access";
        controls = controls.push(
            button(
                row![
                    w::themed_icon(Icon::Shield, ICON_XS, move |c| if is_full { c.danger } else { c.muted }),
                    text(perm_label).size(TEXT_SMALL).font(fonts::MEDIUM),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            )
            .padding([4.0, S])
            .style(w::chip_style(app.composer_popover == Some(ComposerPopover::Permissions)))
            .on_press(Message::ComposerPopover(Some(ComposerPopover::Permissions))),
        );
    }

    if !comp.limits_chip.is_empty() {
        controls = controls.push(
            button(w::colored(&comp.limits_chip, TEXT_CAPTION, |c| c.warning))
                .padding([3.0, S])
                .style(w::chip_style(app.composer_popover == Some(ComposerPopover::Limits)))
                .on_press(Message::ComposerPopover(Some(ComposerPopover::Limits))),
        );
    }

    controls = controls.push(Space::new().width(Length::Fill));

    if !app.draft.text().trim().is_empty() {
        controls = controls.push(w::icon_button(Icon::Archive, "Stash draft", Some(core(Event::StashDraft))));
    }
    if !comp.stash.is_empty() {
        controls = controls.push(
            button(w::muted(format!("Stash ({})", comp.stash.len()), TEXT_CAPTION))
                .padding([4.0, S])
                .style(w::chip_style(app.composer_popover == Some(ComposerPopover::Stash)))
                .on_press(Message::ComposerPopover(Some(ComposerPopover::Stash))),
        );
    }

    if comp.can_steer && !app.draft.text().trim().is_empty() {
        controls = controls.push(w::small_button("Steer", w::secondary_style, Some(core(Event::Steer))));
    }
    if comp.can_force_stop {
        controls = controls.push(w::small_button("Force Stop", w::danger_style, Some(core(Event::ForceStop))));
    }
    if comp.can_stop {
        controls = controls.push(w::icon_label_button(Icon::Square, "Stop", w::Kind::Danger, Some(core(Event::Interrupt))));
    }
    if comp.can_send || !comp.can_stop {
        let send_label = if comp.sending {
            "Sending…"
        } else if comp.queues_offline || comp.can_stop {
            "Queue"
        } else {
            "Send"
        };
        controls = controls.push(w::icon_label_button(
            Icon::Send,
            send_label,
            w::Kind::Primary,
            comp.can_send.then(|| core(Event::Send)),
        ));
    }

    panel_col = panel_col.push(controls);

    if !comp.send_hint.is_empty() {
        panel_col = panel_col.push(w::faint(&comp.send_hint, TEXT_CAPTION));
    }

    let card = container(panel_col)
        .padding(M)
        .width(Length::Fill)
        .style(move |t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.raised)),
                text_color: Some(c.text),
                border: Border {
                    radius: RADIUS_BUBBLE.into(),
                    width: if planning { 1.5 } else { 1.0 },
                    color: if planning { alpha(c.violet, 0.7) } else { c.border },
                },
                ..Default::default()
            }
        });

    stack_col = stack_col.push(card);

    container(stack_col)
        .center_x(Length::Fill)
        .padding(Padding { top: XS, bottom: M, left: L, right: L })
        .into()
}

fn mentions_popover(m: &domovoi_core::view::MentionView) -> Element<'_, Message> {
    let mut col = Column::new().spacing(XXS);
    for (idx, entry) in m.suggestions.iter().take(LIST_LIMIT).enumerate() {
        let selected = idx == m.highlighted as usize;
        col = col.push(
            w::list_row(
                row![
                    w::themed_icon(if entry.directory { Icon::Folder } else { Icon::FileText }, ICON_SMALL, |c| c.muted),
                    text(&entry.name).size(TEXT_UI).font(fonts::MEDIUM),
                    w::faint(&entry.path, TEXT_SMALL).width(Length::Fill),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
                selected,
                false,
            )
            .on_press(core(Event::PickMention { path: entry.path.clone() })),
        );
    }
    container(w::scroll(col).height(Length::Shrink)).max_height(240.0).padding(XS).width(Length::Fill).style(w::popover_style).into()
}

fn commands_popover<'a>(
    items: &'a [domovoi_core::view::SlashCommandView],
    sigil: &'static str,
    highlighted: usize,
    pick: impl Fn(String) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let mut col = Column::new().spacing(XXS);
    for (idx, item) in items.iter().take(LIST_LIMIT).enumerate() {
        col = col.push(
            w::list_row(
                row![
                    text(format!("{sigil}{}", item.name)).size(TEXT_UI).font(fonts::MONO_BOLD),
                    if !item.hint.is_empty() { w::faint(&item.hint, TEXT_SMALL) } else { w::faint("", TEXT_SMALL) },
                    w::muted(&item.description, TEXT_SMALL).width(Length::Fill),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
                idx == highlighted,
                false,
            )
            .on_press(pick(item.name.clone())),
        );
    }
    container(w::scroll(col).height(Length::Shrink)).max_height(240.0).padding(XS).width(Length::Fill).style(w::popover_style).into()
}

fn option_popover(app: &App, pop: ComposerPopover) -> Element<'_, Message> {
    let comp = &app.vm.composer;
    let body: Element<'_, Message> = match pop {
        ComposerPopover::Model => {
            let mut col = Column::new().spacing(XS);
            col = col.push(
                row![
                    w::input("Search models…", &comp.model_query)
                        .on_input(|q| core(Event::SetModelQuery { query: q }))
                        .width(Length::Fill),
                    w::small_button(
                        if comp.show_hidden_models { "Hide hidden" } else { "Show hidden" },
                        w::ghost_style,
                        Some(core(Event::SetShowHiddenModels { show: !comp.show_hidden_models })),
                    ),
                    w::icon_button(Icon::X, "Close", Some(Message::ComposerPopover(None))),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            );
            let mut list = Column::new().spacing(XXS);
            if !comp.favorite_models.is_empty() {
                list = list.push(w::section("Favorites"));
                for m in &comp.favorite_models {
                    list = list.push(model_row(m));
                }
            }
            for grp in &comp.model_groups {
                list = list.push(w::section(&grp.provider));
                if !grp.message.is_empty() {
                    list = list.push(container(w::faint(&grp.message, TEXT_SMALL)).padding([XXS, S]));
                }
                for m in &grp.models {
                    list = list.push(model_row(m));
                }
            }
            col = col.push(w::scroll(list).height(Length::Fixed(300.0)));
            // Model traits (fast mode, context window, etc.)
            if !comp.traits.is_empty() {
                col = col.push(w::divider());
                for tr in &comp.traits {
                    let mut tr_row = row![
                        column![text(&tr.label).size(TEXT_SMALL).font(fonts::MEDIUM), w::faint(&tr.description, TEXT_CAPTION)]
                            .width(Length::Fill),
                    ]
                    .spacing(S)
                    .align_y(Alignment::Center);
                    if tr.kind == "switch" {
                        tr_row = tr_row.push(w::small_button(
                            if tr.on { "On" } else { "Off" },
                            if tr.on { w::primary_style } else { w::secondary_style },
                            Some(core(Event::SetModelOption {
                                id: tr.id.clone(),
                                value: if tr.on { "false" } else { "true" }.into(),
                            })),
                        ));
                    } else {
                        for ch in &tr.choices {
                            let active = ch.value == tr.value;
                            tr_row = tr_row.push(w::small_button(
                                &ch.label,
                                if active { w::primary_style } else { w::secondary_style },
                                Some(core(Event::SetModelOption { id: tr.id.clone(), value: ch.value.clone() })),
                            ));
                        }
                    }
                    col = col.push(tr_row);
                }
            }
            col.into()
        }
        ComposerPopover::Effort => {
            let mut col = Column::new().spacing(XS).push(w::heading(&comp.effort_label));
            for e in &comp.efforts {
                let selected = comp.selected_effort.as_deref() == Some(e.value.as_str());
                col = col.push(
                    w::list_row(text(&e.label).size(TEXT_UI), selected, false)
                        .on_press(core(Event::SelectEffort { value: e.value.clone() })),
                );
            }
            col.into()
        }
        ComposerPopover::Permissions => {
            let mut col = Column::new().spacing(XS).push(w::heading("Permissions"));
            for mode in &comp.runtime_modes {
                let selected = comp.runtime_mode == mode.value;
                col = col.push(
                    w::list_row(text(&mode.label).size(TEXT_UI), selected, false)
                        .on_press(Message::SetPermissionsRequested(mode.value.clone())),
                );
            }
            col.into()
        }
        ComposerPopover::Stash => {
            let mut col = Column::new().spacing(XS).push(w::heading("Stashed drafts"));
            for (idx, line) in comp.stash.iter().enumerate() {
                col = col.push(
                    row![
                        w::list_row(text(line).size(TEXT_SMALL), false, false)
                            .on_press(core(Event::RestoreStash { index: idx as u32 })),
                        w::icon_button(Icon::Trash2, "Drop stash", Some(core(Event::DropStash { index: idx as u32 }))),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                );
            }
            col.into()
        }
        ComposerPopover::Limits => {
            let mut col = Column::new().spacing(XS).push(w::heading("Provider limits"));
            for w_lim in &comp.limits {
                col = col.push(
                    row![
                        text(&w_lim.label).size(TEXT_SMALL).font(fonts::MEDIUM).width(Length::Fill),
                        w::colored(format!("{}%", w_lim.percent), TEXT_SMALL, if w_lim.high { |c| c.warning } else { |c| c.muted }),
                        if !w_lim.resets.is_empty() { w::faint(format!("· {}", w_lim.resets), TEXT_CAPTION) } else { w::faint("", TEXT_CAPTION) },
                    ]
                    .spacing(S)
                    .align_y(Alignment::Center),
                );
            }
            col.into()
        }
    };
    container(body).padding(M).width(Length::Fill).style(w::popover_style).into()
}

fn model_row(m: &domovoi_core::view::ModelRowView) -> Element<'_, Message> {
    let fav = m.favorite;
    let hidden = m.hidden;
    row![
        w::list_row(
            column![
                text(&m.name).size(TEXT_UI).font(if m.chosen { fonts::SEMIBOLD } else { fonts::INTER }),
                if !m.detail.is_empty() { w::faint(&m.detail, TEXT_CAPTION) } else { w::faint("", TEXT_CAPTION) },
            ]
            .spacing(1.0)
            .width(Length::Fill),
            m.chosen,
            !m.detail.is_empty(),
        )
        .on_press(core(Event::SelectModel {
            instance_id: m.option.instance_id.clone(),
            model: m.option.model.clone(),
        })),
        w::icon_button_tinted(
            Icon::Star,
            if fav { "Unfavorite" } else { "Favorite" },
            Some(core(Event::SetModelFavorite {
                instance_id: m.option.instance_id.clone(),
                model: m.option.model.clone(),
                favorite: !fav,
            })),
            fav,
        ),
        w::icon_button(
            if hidden { Icon::Eye } else { Icon::EyeOff },
            if hidden { "Show model" } else { "Hide model" },
            Some(core(Event::SetModelHidden {
                instance_id: m.option.instance_id.clone(),
                model: m.option.model.clone(),
                hidden: !hidden,
            })),
        ),
    ]
    .spacing(XXS)
    .align_y(Alignment::Center)
    .into()
}
