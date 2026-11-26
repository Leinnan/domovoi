//! The message box under the thread, and what goes with a message: its
//! pictures, the model and its options, the permissions, plan mode.
//!
//! Enter sends, Shift+Enter starts a new line. Typing `@` lists the project's
//! files, `/` the commands and `$` the skills: the arrows move through the
//! list and Enter or Tab takes one. On a draft of one line the arrows bring
//! back earlier messages instead.
//!
//! The box shows the window's own copy of the draft, not the core's: the
//! core's comes back a moment after each key, and setting a text box to it
//! would move the caret. The core's replaces it only when the core itself
//! changed the draft (`draft_revision`: sent, restored, a command taken).

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ComposerView, ModelRowView, SlashCommandView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, DragDropAction, DragDropOperation, DragDropPolicy, DroppedData, ElementRef, FontWeight, Grid, GridLength,
    HorizontalAlignment, InputModifiers, Key, KeyEventInfo, Menu, MenuExt as _, MenuItem, Orientation, ProgressRing, RoutedCallback, ScrollBarVisibility, ScrollViewer,
    StackPanel, TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, ToggleSwitch, TooltipExt as _, VerticalAlignment, View, VirtualKey,
};

use crate::core_link::CoreLink;
use crate::theme;

/// The most rows a list above the box shows (and the arrows go through).
pub const LIST_LIMIT: usize = 12;

/// What the composer's keys do now, beyond typing. The window says, each
/// time it draws; the key handler reads it when a key comes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// A list of files, commands or skills is open.
    pub list: bool,
    /// It is the list of files, which Escape shuts.
    pub mention: bool,
    /// The arrows bring back earlier messages (a draft of one line, in a thread).
    pub recall: bool,
    /// The provider has plan mode: Shift+Tab switches it.
    pub plan: bool,
}

/// What a key in the box asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Take the marked row of the open list, else send.
    Enter,
    /// Move the mark in the open list.
    Move(i32),
    /// An earlier message of this thread (-1), or a later one.
    Recall(i32),
    TogglePlan,
    /// Shut the list of files.
    Dismiss,
    /// Ctrl+V: a picture on the clipboard is attached (text pastes as ever).
    Paste,
}

const V: VirtualKey = VirtualKey(0x56);

/// What `key` does in the box, and whether the box is kept from having it.
pub fn key_action(key: VirtualKey, modifiers: InputModifiers, keys: Keys) -> Option<(KeyAction, bool)> {
    let held = |modifier| modifiers.contains(modifier);
    let (ctrl, shift, alt) = (held(InputModifiers::CONTROL), held(InputModifiers::SHIFT), held(InputModifiers::ALT));
    let plain = !ctrl && !shift && !alt;
    match key {
        // Shift+Enter is the box's own: a new line.
        VirtualKey::ENTER if !shift && !alt => Some((KeyAction::Enter, true)),
        VirtualKey::TAB if plain && keys.list => Some((KeyAction::Enter, true)),
        VirtualKey::TAB if shift && !ctrl && !alt && !keys.list && keys.plan => Some((KeyAction::TogglePlan, true)),
        VirtualKey::UP | VirtualKey::DOWN if plain && (keys.list || keys.recall) => {
            let delta = if key == VirtualKey::UP { -1 } else { 1 };
            Some((if keys.list { KeyAction::Move(delta) } else { KeyAction::Recall(delta) }, true))
        }
        VirtualKey::ESCAPE if plain && keys.mention => Some((KeyAction::Dismiss, true)),
        V if ctrl && !shift && !alt => Some((KeyAction::Paste, false)),
        _ => None,
    }
}

/// The row a list's mark is on after moving it by `delta`, going round.
pub fn moved(mark: usize, delta: i32, rows: usize) -> usize {
    let rows = rows.min(LIST_LIMIT);
    if rows == 0 { 0 } else { (mark as i64 + i64::from(delta)).rem_euclid(rows as i64) as usize }
}

/// What the composer tells the window.
pub struct Hooks<'a> {
    /// The message box, so the window can put the keyboard in it.
    pub field: &'a ElementRef<TextBox>,
    pub on_draft: Callback<Rc<str>>,
    /// Made once by the window: the box's keys (`key_action`).
    pub on_keys: RoutedCallback<KeyEventInfo>,
    /// A choice from a chip's menu: `effort:<value>`, `perm:<mode>`, `stash-restore:<index>`, …
    pub on_menu: Callback<Key>,
    /// The model search's text.
    pub on_model_query: Callback<Rc<str>>,
    /// Opens or shuts the models.
    pub on_models: Callback<()>,
    /// Pick pictures to attach.
    pub on_attach: Callback<()>,
    /// Files dropped on the composer.
    pub on_drop: Callback<DroppedData>,
}

/// What the window keeps for the composer.
pub struct Local<'a> {
    /// The draft as typed (the window's copy).
    pub draft: &'a str,
    pub models_open: bool,
    /// The model search as typed.
    pub model_query: &'a str,
    /// The row marked in the list of commands or skills.
    pub mark: usize,
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

/// A panel above the box: a list to pick from, or the models.
fn panel(content: View) -> View {
    Border::new()
        .margin(Thickness::new(0.0, 0.0, 0.0, 6.0))
        .padding(Thickness::uniform(6.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(content)
        .into()
}

fn list(rows: Vec<View>, height: f64) -> View {
    ScrollViewer::new()
        .max_height(height)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(1.0).children(rows))
        .into()
}

/// A row of a list: pressed, `event` goes to the core. `marked`: the arrows are on it.
fn pick(core: &CoreLink, name: &str, marked: bool, content: View, event: Event) -> View {
    let core = core.clone();
    let button = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Left)
        .automation_name(name)
        .content(content)
        .on_click(move || core.send(event.clone()));
    if marked { button.background(theme::selected()) } else { button }.into()
}

/// The files `@` finds, or the commands (`/`) or skills (`$`) being typed.
fn suggestions(core: &CoreLink, composer: &ComposerView, mark: usize) -> Option<View> {
    if let Some(mention) = composer.mention.as_ref().filter(|m| !m.suggestions.is_empty()) {
        let rows = mention.suggestions.iter().take(LIST_LIMIT).enumerate().map(|(index, entry)| {
            let content = StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                theme::glyph(if entry.directory { "\u{E8B7}" } else { "\u{E7C3}" }, 13.0).opacity(0.7).into(),
                TextBlock::new().text(&entry.name).into(),
                caption(&entry.path).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
            ]);
            pick(core, &entry.path, index == mention.highlighted as usize, content.into(), Event::PickMention { path: entry.path.clone() })
        });
        return Some(panel(list(rows.collect(), 240.0)));
    }
    let commands = |items: &[SlashCommandView], sigil: &str, event: fn(String) -> Event| {
        let rows = items.iter().take(LIST_LIMIT).enumerate().map(|(index, item)| {
            let name = format!("{sigil}{}", item.name);
            let mut cells: Vec<View> = vec![TextBlock::new().text(&name).font_family(theme::MONO).font_weight(FontWeight::SEMI_BOLD).into()];
            if !item.hint.is_empty() {
                cells.push(caption(&item.hint).font_family(theme::MONO).vertical_alignment(VerticalAlignment::Center).into());
            }
            cells.push(caption(&item.description).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into());
            pick(core, &name, index == mark, StackPanel::new().orientation(Orientation::Horizontal).spacing(10.0).children(cells).into(), event(item.name.clone()))
        });
        panel(list(rows.collect(), 240.0))
    };
    if !composer.skills.is_empty() {
        Some(commands(&composer.skills, "$", |name| Event::PickSkill { name }))
    } else if !composer.slash_commands.is_empty() {
        Some(commands(&composer.slash_commands, "/", |name| Event::PickSlashCommand { name }))
    } else {
        None
    }
}

/// A model: pressed, the thread uses it. Beside it, its star and whether the list shows it.
fn model_row(core: &CoreLink, model: &ModelRowView) -> View {
    let (instance_id, id) = (&model.option.instance_id, &model.option.model);
    let mut name: Vec<View> = vec![TextBlock::new().text(&model.name).font_weight(if model.chosen { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }).text_trimming(TextTrimming::CharacterEllipsis).into()];
    if !model.detail.is_empty() {
        name.push(caption(&model.detail).into());
    }
    let mut cells: Vec<View> = vec![StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(name).into()];
    if model.chosen {
        cells.push(theme::glyph("\u{E73E}", 12.0).foreground(theme::accent()).into());
    }
    let choose = pick(
        core,
        &model.name,
        model.chosen,
        Grid::new().columns([GridLength::Pixel(18.0), GridLength::STAR]).children(cells).into(),
        Event::SelectModel { instance_id: instance_id.clone(), model: id.clone() },
    );
    let small = |glyph: &str, name: String, on: bool, event: Event| -> View {
        let core = core.clone();
        let glyph = theme::glyph(glyph, 12.0);
        Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name(&name)
            .content(if on { glyph.foreground(theme::accent()) } else { glyph.opacity(0.6) })
            .on_click(move || core.send(event.clone()))
            .tooltip(&name)
    };
    let star = small(
        if model.favorite { "\u{E735}" } else { "\u{E734}" },
        format!("{} {}", if model.favorite { "Remove the star of" } else { "Star" }, model.name),
        model.favorite,
        Event::SetModelFavorite { instance_id: instance_id.clone(), model: id.clone(), favorite: !model.favorite },
    );
    let hide = small(
        if model.hidden { "\u{E7B3}" } else { "\u{ED1A}" },
        format!("{} {}", if model.hidden { "Show" } else { "Hide" }, model.name),
        model.hidden,
        Event::SetModelHidden { instance_id: instance_id.clone(), model: id.clone(), hidden: !model.hidden },
    );
    Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto]).children(vec![choose, Border::new().grid_column(1).content(star).into(), Border::new().grid_column(2).content(hide).into()]).into()
}

/// The models to choose from, found by typing, with the chosen one's options under them.
fn models(core: &CoreLink, composer: &ComposerView, local: &Local<'_>, hooks: &Hooks<'_>) -> View {
    let heading = |text: &str| -> View { caption(text).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(10.0, 8.0, 0.0, 2.0)).into() };
    let hidden = {
        let (core, show) = (core.clone(), composer.show_hidden_models);
        Button::new().grid_column(1).content(TextBlock::new().text(if show { "Hide Hidden" } else { "Show Hidden" })).on_click(move || core.send(Event::SetShowHiddenModels { show: !show }))
    };
    let close = {
        let on_models = hooks.on_models.clone();
        Button::new().grid_column(2).style(ButtonStyle::Subtle).automation_name("Close the models").content(theme::glyph("\u{E711}", 12.0)).on_click(move || on_models.call(()))
    };
    let mut rows: Vec<View> = Vec::new();
    if !composer.favorite_models.is_empty() {
        rows.push(heading("Favorites"));
        rows.extend(composer.favorite_models.iter().map(|m| model_row(core, m)));
    }
    for group in &composer.model_groups {
        if group.models.is_empty() && group.message.is_empty() {
            continue;
        }
        rows.push(heading(&group.provider));
        if !group.message.is_empty() {
            rows.push(caption(&group.message).text_wrapping(TextWrapping::WrapWholeWords).max_lines(3).text_trimming(TextTrimming::CharacterEllipsis).margin(Thickness::new(10.0, 0.0, 10.0, 4.0)).tooltip(&group.message));
        }
        rows.extend(group.models.iter().map(|m| model_row(core, m)));
    }
    if rows.is_empty() {
        rows.push(caption(if composer.model_query.is_empty() { "No models: sign in to a provider in Settings." } else { "No models match." }).margin(Thickness::uniform(10.0)).into());
    }
    let mut parts: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                TextBox::new().automation_name("Search models").placeholder_text("Search models").text(local.model_query).on_text_changed(hooks.on_model_query.clone()),
                hidden,
                close,
            ))
            .into(),
        list(rows, 280.0),
    ];
    // The chosen model's other options: fast mode, the context window…
    for option in &composer.traits {
        let control: View = if option.kind == "switch" {
            let (core, id, on) = (core.clone(), option.id.clone(), option.on);
            ToggleSwitch::new()
                .min_width(0.0)
                .is_on(on)
                .on_content(TextBlock::new())
                .off_content(TextBlock::new())
                .automation_name(&option.label)
                .on_toggled(move |to: bool| {
                    if to != on {
                        core.send(Event::SetModelOption { id: id.clone(), value: to.to_string() });
                    }
                })
                .into()
        } else {
            let chosen = option.choices.iter().position(|c| c.value == option.value);
            let (core, id, values) = (core.clone(), option.id.clone(), option.choices.iter().map(|c| c.value.clone()).collect::<Vec<_>>());
            ComboBox::new()
                .automation_name(&option.label)
                .min_width(160.0)
                .items_source(option.choices.iter().map(|c| c.label.clone()))
                .selected_index(chosen)
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some(value) = index.filter(|i| Some(*i) != chosen).and_then(|i| values.get(i)) {
                        core.send(Event::SetModelOption { id: id.clone(), value: value.clone() });
                    }
                })
                .into()
        };
        let mut name: Vec<View> = vec![TextBlock::new().text(&option.label).into()];
        if !option.description.is_empty() {
            name.push(caption(&option.description).text_wrapping(TextWrapping::WrapWholeWords).into());
        }
        parts.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(12.0)
                .margin(Thickness::new(10.0, 2.0, 6.0, 2.0))
                .children((StackPanel::new().orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Center).children(name), Border::new().grid_column(1).vertical_alignment(VerticalAlignment::Center).content(control)))
                .into(),
        );
    }
    panel(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(parts).into())
}

/// A chip under the box: a button with a word (and a glyph) on it.
fn chip(glyph: Option<&str>, label: &str, name: &str) -> Button {
    let mut cells: Vec<View> = Vec::new();
    if let Some(glyph) = glyph {
        cells.push(theme::glyph(glyph, 12.0).opacity(0.8).into());
    }
    cells.push(TextBlock::new().text(label).font_size(theme::CAPTION).vertical_alignment(VerticalAlignment::Center).text_trimming(TextTrimming::CharacterEllipsis).max_width(220.0).into());
    Button::new().automation_name(name).content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(cells))
}

/// A menu of choices with the one in force ticked.
fn choices(what: &str, items: impl Iterator<Item = (String, String, bool)>, on_menu: &Callback<Key>) -> Menu {
    Menu::new(items.map(|(value, label, chosen)| MenuItem::item(format!("{what}:{value}"), format!("{}{label}", if chosen { "✓  " } else { "     " }))).collect::<Vec<_>>(), on_menu.clone())
}

pub fn composer(core: &CoreLink, composer: &ComposerView, local: &Local<'_>, hooks: Hooks<'_>) -> View {
    let planning = composer.interaction_mode == "plan";
    let typed = !local.draft.trim().is_empty();
    let mut above: Vec<View> = Vec::new();
    if let Some(list) = suggestions(core, composer, local.mark) {
        above.push(list);
    } else if local.models_open {
        above.push(models(core, composer, local, &hooks));
    }

    let mut lines: Vec<View> = Vec::new();
    if !composer.attachments.is_empty() {
        let pictures = composer.attachments.iter().map(|attachment| {
            let (core, id) = (core.clone(), attachment.id.clone());
            let lead: View = if attachment.uploading { ProgressRing::new().is_active(true).width(12.0).height(12.0).into() } else { theme::glyph("\u{EB9F}", 12.0).opacity(0.8).into() };
            let remove = Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("Remove {}", attachment.name))
                .content(theme::glyph("\u{E711}", 10.0))
                .on_click(move || core.send(Event::RemoveAttachment { id: id.clone() }));
            Border::new()
                .padding(Thickness::new(8.0, 0.0, 0.0, 0.0))
                .corner_radius(6.0)
                .background(theme::code())
                .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(vec![
                    lead,
                    TextBlock::new().text(&attachment.name).font_size(theme::CAPTION).vertical_alignment(VerticalAlignment::Center).max_width(180.0).text_trimming(TextTrimming::CharacterEllipsis).into(),
                    caption(if attachment.uploading { "Uploading…" } else { attachment.size_label.as_str() }).vertical_alignment(VerticalAlignment::Center).into(),
                    remove.into(),
                ]))
                .into()
        });
        lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(pictures.collect::<Vec<View>>()).into());
    }
    if !composer.blocked_reason.is_empty() {
        // A server's reason can run to a screenful (a whole PATH): a few lines here, all of it on hover.
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![
                    theme::glyph("\u{E7BA}", 13.0).foreground(theme::critical()).vertical_alignment(VerticalAlignment::Top).into(),
                    TextBlock::new().text(&composer.blocked_reason).font_size(theme::CAPTION).text_wrapping(TextWrapping::WrapWholeWords).max_lines(3).text_trimming(TextTrimming::CharacterEllipsis).tooltip(&composer.blocked_reason),
                ])
                .into(),
        );
    }
    let placeholder = if composer.placeholder.is_empty() { "Message the agent (Enter sends, Shift+Enter starts a new line)" } else { composer.placeholder.as_str() };
    lines.push(
        Border::new()
            .on_preview_key_down(hooks.on_keys)
            .content(
                TextBox::new()
                    .element_ref(hooks.field)
                    .automation_name("Message")
                    .placeholder_text(placeholder)
                    .accepts_return(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .min_height(44.0)
                    .max_height(220.0)
                    .text(local.draft)
                    .on_text_changed(hooks.on_draft),
            )
            .into(),
    );

    // Under the box: what the message goes with, then what to do with it.
    let mut left: Vec<View> = Vec::new();
    let attach = {
        let on_attach = hooks.on_attach;
        Button::new().style(ButtonStyle::Subtle).automation_name("Attach pictures").content(theme::glyph("\u{E723}", 14.0)).on_click(move || on_attach.call(())).tooltip("Attach pictures (or drop or paste them)")
    };
    left.push(attach);
    let model = composer.selected_model.as_ref().map(|m| m.label.as_str()).filter(|label| !label.is_empty()).unwrap_or("Choose a model");
    let on_models = hooks.on_models.clone();
    left.push(chip(Some("\u{E99A}"), model, &format!("Model: {model}")).on_click(move || on_models.call(())).tooltip("The model, and its options"));
    if !composer.efforts.is_empty() {
        let chosen = composer.selected_effort.as_deref();
        let label = composer.efforts.iter().find(|e| Some(e.value.as_str()) == chosen).map_or(composer.effort_label.as_str(), |e| e.label.as_str());
        let menu = choices("effort", composer.efforts.iter().map(|e| (e.value.clone(), e.label.clone(), Some(e.value.as_str()) == chosen)), &hooks.on_menu);
        left.push(chip(None, label, &format!("{}: {label}", composer.effort_label)).menu(menu));
    }
    if composer.shows_interaction_mode || planning {
        let core = core.clone();
        left.push(
            chip(Some("\u{E9D5}"), "Plan", if planning { "Plan mode is on" } else { "Plan mode is off" })
                .style(if planning { ButtonStyle::Accent } else { ButtonStyle::Default })
                .on_click(move || core.send(Event::SetInteractionMode { mode: if planning { "default" } else { "plan" }.into() }))
                .tooltip("Plan first: the agent proposes a plan before it acts (Shift+Tab)"),
        );
    }
    if !composer.runtime_modes.is_empty() {
        let label = composer.runtime_modes.iter().find(|m| m.value == composer.runtime_mode).map_or(composer.runtime_mode.as_str(), |m| m.label.as_str());
        let menu = choices("perm", composer.runtime_modes.iter().map(|m| (m.value.clone(), m.label.clone(), m.value == composer.runtime_mode)), &hooks.on_menu);
        left.push(chip(Some("\u{E72E}"), label, &format!("Permissions: {label}")).menu(menu));
    }
    if !composer.limits_chip.is_empty() {
        let windows = composer.limits.iter().map(|w| if w.resets.is_empty() { format!("{} {}%", w.label, w.percent) } else { format!("{} {}%, resets {}", w.label, w.percent, w.resets) });
        left.push(
            Border::new()
                .padding(Thickness::xy(8.0, 2.0))
                .corner_radius(10.0)
                .background(theme::code())
                .vertical_alignment(VerticalAlignment::Center)
                .content(TextBlock::new().text(&composer.limits_chip).font_size(theme::CAPTION).foreground(theme::critical()))
                .tooltip(windows.collect::<Vec<_>>().join("\n")),
        );
    }

    let tell = |label: &str, event: Event, tip: &str| -> View {
        let core = core.clone();
        Button::new().vertical_alignment(VerticalAlignment::Bottom).content(TextBlock::new().text(label)).on_click(move || core.send(event.clone())).tooltip(tip)
    };
    let mut right: Vec<View> = Vec::new();
    if typed {
        let core = core.clone();
        right.push(Button::new().style(ButtonStyle::Subtle).automation_name("Put the draft aside").content(theme::glyph("\u{E7B8}", 14.0)).on_click(move || core.send(Event::StashDraft)).tooltip("Put the draft aside for later"));
    }
    if !composer.stash.is_empty() {
        let items = composer.stash.iter().enumerate().map(|(index, line)| {
            let line: String = line.chars().take(60).collect();
            MenuItem::submenu(format!("stash:{index}"), line, [MenuItem::item(format!("stash-restore:{index}"), "Take Back"), MenuItem::item(format!("stash-drop:{index}"), "Drop")])
        });
        let label = format!("Aside ({})", composer.stash.len());
        right.push(chip(None, &label, "Drafts put aside").menu(Menu::new(items.collect::<Vec<_>>(), hooks.on_menu.clone())));
    }
    if composer.can_steer && typed {
        right.push(tell("Steer", Event::Steer, "Send it into the turn under way, not after it"));
    }
    if composer.can_force_stop {
        right.push(tell("Force Stop", Event::ForceStop, "End the turn even if the agent does not stop"));
    }
    if composer.can_stop {
        let core = core.clone();
        right.push(Button::new().automation_name("Stop").vertical_alignment(VerticalAlignment::Bottom).content(theme::glyph("\u{E71A}", 14.0)).on_click(move || core.send(Event::Interrupt)).tooltip("Stop (Ctrl+.)"));
    }
    if composer.can_send || !composer.can_stop {
        let core = core.clone();
        // While a turn runs, or with no connection, what is sent waits its turn.
        let queues = composer.queues_offline || composer.can_stop;
        let tip = if composer.sending {
            "Sending…"
        } else if queues {
            "Queue it (Enter)"
        } else {
            "Send (Enter)"
        };
        right.push(
            Button::new()
                .style(ButtonStyle::Accent)
                .automation_name(if queues { "Queue" } else { "Send" })
                .is_enabled(composer.can_send && !composer.sending)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(theme::glyph(if queues { "\u{E823}" } else { "\u{E724}" }, 14.0))
                .on_click(move || core.send(Event::Send))
                .tooltip(tip),
        );
    }
    lines.push(
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(left),
                StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(6.0).children(right),
            ))
            .into(),
    );
    if !composer.send_hint.is_empty() {
        lines.push(caption(&composer.send_hint).text_wrapping(TextWrapping::WrapWholeWords).into());
    }

    above.push(
        Border::new()
            .padding(Thickness::uniform(8.0))
            .background(theme::card())
            .border_brush(if planning { theme::accent() } else { theme::stroke() })
            .border_thickness(Thickness::uniform(1.0))
            .corner_radius(theme::RADIUS)
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines))
            .into(),
    );
    Border::new()
        .margin(Thickness::new(theme::GAP * 2.0, 0.0, theme::GAP * 2.0, theme::GAP * 2.0))
        .drop_policy(DragDropPolicy::new().storage_items(DragDropAction::new(DragDropOperation::Copy).caption("Attach")))
        .on_drop(hooks.on_drop)
        .content(StackPanel::new().orientation(Orientation::Vertical).children(above))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(modifiers: &[InputModifiers]) -> InputModifiers {
        let mut all = InputModifiers::NONE;
        for modifier in modifiers {
            all |= *modifier;
        }
        all
    }

    #[test]
    fn enter_sends_and_shift_enter_is_left_to_the_box() {
        let keys = Keys::default();
        assert_eq!(key_action(VirtualKey::ENTER, InputModifiers::NONE, keys), Some((KeyAction::Enter, true)));
        assert_eq!(key_action(VirtualKey::ENTER, with(&[InputModifiers::CONTROL]), keys), Some((KeyAction::Enter, true)), "Ctrl+Enter sends too");
        assert_eq!(key_action(VirtualKey::ENTER, with(&[InputModifiers::SHIFT]), keys), None);
    }

    #[test]
    fn the_arrows_are_the_boxs_unless_a_list_is_open_or_a_message_can_come_back() {
        assert_eq!(key_action(VirtualKey::UP, InputModifiers::NONE, Keys::default()), None, "in a draft of several lines the arrows move the caret");
        let list = Keys { list: true, mention: true, recall: true, plan: true };
        assert_eq!(key_action(VirtualKey::DOWN, InputModifiers::NONE, list), Some((KeyAction::Move(1), true)));
        assert_eq!(key_action(VirtualKey::TAB, InputModifiers::NONE, list), Some((KeyAction::Enter, true)));
        assert_eq!(key_action(VirtualKey::ESCAPE, InputModifiers::NONE, list), Some((KeyAction::Dismiss, true)));
        assert_eq!(key_action(VirtualKey::TAB, with(&[InputModifiers::SHIFT]), list), None, "not while a list is open");
        let recall = Keys { list: false, mention: false, recall: true, plan: true };
        assert_eq!(key_action(VirtualKey::ESCAPE, InputModifiers::NONE, Keys { list: true, ..recall }), None, "a list of commands goes when its word does");
        assert_eq!(key_action(VirtualKey::UP, InputModifiers::NONE, recall), Some((KeyAction::Recall(-1), true)));
        assert_eq!(key_action(VirtualKey::TAB, with(&[InputModifiers::SHIFT]), recall), Some((KeyAction::TogglePlan, true)));
        assert_eq!(key_action(VirtualKey::TAB, InputModifiers::NONE, recall), None, "Tab moves on as ever");
        assert_eq!(key_action(VirtualKey::ESCAPE, InputModifiers::NONE, recall), None);
    }

    #[test]
    fn pasting_is_seen_and_left_to_the_box() {
        assert_eq!(key_action(V, with(&[InputModifiers::CONTROL]), Keys::default()), Some((KeyAction::Paste, false)));
        assert_eq!(key_action(V, InputModifiers::NONE, Keys::default()), None);
    }

    #[test]
    fn the_mark_goes_round_the_rows_shown() {
        assert_eq!(moved(0, -1, 3), 2);
        assert_eq!(moved(2, 1, 3), 0);
        assert_eq!(moved(11, 1, 40), 0, "only the rows shown");
        assert_eq!(moved(5, 1, 0), 0);
    }
}
