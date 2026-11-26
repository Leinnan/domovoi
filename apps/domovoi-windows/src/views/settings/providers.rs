//! Settings: the agents the server can run (their sign-in and runtimes), the
//! server's keyboard shortcuts, and its scheduled tasks.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ProviderView, ScheduledTaskView, SettingsView};
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, InfoBar, InfoBarSeverity, Orientation, ProgressBar, StackPanel, TextBlock, TextBox,
    TextTrimming, TextWrapping, Thickness, ToggleSwitch, VerticalAlignment, View,
};

use super::{Hooks, Shown, caption, card, empty};
use crate::core_link::CoreLink;
use crate::theme;

/// A button that tells the core `event`.
fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label)).on_click(move || core.send(event.clone()))
}

/// A button that opens a dialog or acts through the window: `key` as the window's menu takes it.
fn ask(hooks: &Hooks<'_>, label: &str, key: String) -> Button {
    let on_menu = hooks.on_menu.clone();
    Button::new().content(TextBlock::new().text(label)).on_click(move || on_menu.call(key.as_str().into()))
}

fn fact(label: &str, value: &str) -> View {
    Grid::new()
        .columns([GridLength::Pixel(130.0), GridLength::STAR])
        .children((caption(label), TextBlock::new().grid_column(1).text(value).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true)))
        .into()
}

fn buttons(buttons: Vec<View>) -> View {
    StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(buttons).into()
}

/// A word in a pill: a provider's state, a task's.
fn pill(text: &str, good: bool) -> View {
    Border::new()
        .padding(Thickness::xy(8.0, 2.0))
        .corner_radius(10.0)
        .background(theme::code())
        .vertical_alignment(VerticalAlignment::Center)
        .content(TextBlock::new().text(text).font_size(theme::CAPTION).foreground(if good { theme::accent() } else { theme::primary() }))
        .into()
}

pub fn providers(core: &CoreLink, shown: &Shown<'_>, hooks: &Hooks<'_>) -> Vec<View> {
    let settings = shown.settings;
    let mut bar = vec![tell(core, "Check Again", Event::RefreshProviders).into()];
    if settings.can_add_antigravity {
        bar.push(ask(hooks, "Add Antigravity Account…", "provider-add:".to_owned()).into());
    }
    let mut out = vec![buttons(bar)];
    if settings.providers.is_empty() {
        out.push(empty("The server's providers are loading."));
    }
    out.extend(settings.providers.iter().map(|provider| self::provider(core, provider, shown, hooks)));
    out.push(failover_card(core, settings));
    out
}

fn failover_card(core: &CoreLink, settings: &SettingsView) -> View {
    let mut lines: Vec<View> = vec![TextBlock::new().text("Failover Pools & Availability").font_weight(FontWeight::SEMI_BOLD).into()];
    if settings.failover_instances.is_empty() {
        lines.push(caption("No live availability data yet.").into());
    } else {
        for inst in &settings.failover_instances {
            let name = if inst.display_name.is_empty() { inst.instance_id.as_str() } else { inst.display_name.as_str() };
            let mut detail = format!("{} · {}", inst.driver, if inst.available { "available" } else { "rate-limited" });
            if inst.active_jobs > 0 {
                detail.push_str(&format!(" · {} active", inst.active_jobs));
            }
            if let Some(until) = &inst.limited_until {
                detail.push_str(&format!(" · until {until}"));
            }
            if let Some(err) = &inst.last_error {
                detail.push_str(&format!(" · {err}"));
            }
            lines.push(fact(name, &detail));
        }
    }
    if settings.failover_pools.is_empty() && !settings.providers.is_empty() {
        let ids: Vec<String> = settings.providers.iter().map(|p| p.instance_id.clone()).collect();
        lines.push(
            tell(
                core,
                "Create Default Pool",
                Event::SaveFailoverPool {
                    pool_id: "default".into(),
                    name: "Default Pool".into(),
                    instance_ids: ids,
                    same_driver_first: true,
                    allow_cross_provider: true,
                },
            )
            .horizontal_alignment(HorizontalAlignment::Left)
            .into(),
        );
    }
    for pool in &settings.failover_pools {
        let mut rotated = pool.instance_ids.clone();
        if rotated.len() > 1 {
            rotated.rotate_left(1);
        }
        lines.push(TextBlock::new().text(format!("{} ({}): {}", pool.name, pool.id, pool.instance_ids.join(" → "))).font_family(theme::MONO).font_size(theme::CAPTION).into());
        lines.push(buttons(vec![
            tell(
                core,
                if pool.same_driver_first { "Same driver first: On" } else { "Same driver first: Off" },
                Event::SaveFailoverPool {
                    pool_id: pool.id.clone(),
                    name: pool.name.clone(),
                    instance_ids: pool.instance_ids.clone(),
                    same_driver_first: !pool.same_driver_first,
                    allow_cross_provider: pool.allow_cross_provider,
                },
            )
            .into(),
            tell(
                core,
                if pool.allow_cross_provider { "Cross-provider: On" } else { "Cross-provider: Off" },
                Event::SaveFailoverPool {
                    pool_id: pool.id.clone(),
                    name: pool.name.clone(),
                    instance_ids: pool.instance_ids.clone(),
                    same_driver_first: pool.same_driver_first,
                    allow_cross_provider: !pool.allow_cross_provider,
                },
            )
            .into(),
            tell(
                core,
                "Rotate",
                Event::SaveFailoverPool {
                    pool_id: pool.id.clone(),
                    name: pool.name.clone(),
                    instance_ids: rotated,
                    same_driver_first: pool.same_driver_first,
                    allow_cross_provider: pool.allow_cross_provider,
                },
            )
            .is_enabled(pool.instance_ids.len() > 1)
            .into(),
        ]));
    }
    card(lines)
}

/// One agent: what it is, whether it is ready, and what can be done about it.
fn provider(core: &CoreLink, p: &ProviderView, shown: &Shown<'_>, hooks: &Hooks<'_>) -> View {
    let id = &p.instance_id;
    let name = if p.name.is_empty() { id.as_str() } else { p.name.as_str() };
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((TextBlock::new().text(name).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center), Border::new().grid_column(1).content(pill(&p.status, p.status == "ready"))))
            .into(),
    ];
    if !p.version.is_empty() {
        lines.push(fact("Version", &p.version));
    }
    if !p.install.installed_version.is_empty() {
        lines.push(fact("Installed runtime", &p.install.installed_version));
    }
    if p.config.editable && p.config.auth_methods.len() > 1 {
        let method = p.config.auth_methods.iter().find(|m| m.value == p.config.auth_method).map_or(p.config.auth_method.as_str(), |m| m.label.as_str());
        lines.push(fact("Sign-in method", method));
    }
    if p.config.has_api_key {
        lines.push(fact("API key", "Saved"));
    }
    if !p.auth.is_empty() {
        lines.push(fact("Account", &p.auth));
    }
    lines.push(fact("Models", &p.model_count.to_string()));
    if !p.limits.is_empty() {
        lines.push(fact("Limits", &p.limits));
    }
    if !p.message.is_empty() {
        lines.push(caption(&p.message).into());
    }

    let mut actions: Vec<View> = Vec::new();
    if p.can_authenticate && !p.sign_in.busy {
        actions.push(tell(core, if p.signed_in { "Sign In Again" } else { "Sign In" }, Event::StartProviderSignIn { instance_id: id.clone() }).style(ButtonStyle::Accent).automation_name(format!("Sign in to {name}")).into());
    }
    if p.sign_in.busy {
        actions.push(tell(core, "Cancel Sign-In", Event::CancelProviderSignIn { instance_id: id.clone() }).into());
    }
    if p.can_install && !p.install.busy {
        let label = if p.install.update_available { format!("Update to {}", p.install.offered_version) } else { "Install Runtime".to_owned() };
        actions.push(tell(core, &label, Event::InstallProvider { instance_id: id.clone() }).automation_name(format!("{label} for {name}")).into());
    }
    if p.install.busy {
        actions.push(tell(core, "Cancel Install", Event::CancelProviderInstall { instance_id: id.clone() }).into());
    }
    if p.install.can_remove {
        actions.push(tell(core, "Remove Runtime", Event::RemoveProviderInstall { instance_id: id.clone() }).automation_name(format!("Remove the runtime of {name}")).into());
    }
    if p.config.editable {
        actions.push(ask(hooks, "Configure…", format!("provider-edit:{id}")).automation_name(format!("Configure {name}")).into());
        if !p.config.built_in {
            actions.push(ask(hooks, "Remove…", format!("provider-remove:{id}")).automation_name(format!("Remove {name}")).into());
        }
    }
    if !actions.is_empty() {
        lines.push(buttons(actions));
    }

    // A sign-in under way: the page to sign in on, and where its redirect is pasted.
    if !p.sign_in.url.is_empty() || p.sign_in.takes_redirect {
        let mut sign_in: Vec<View> = Vec::new();
        if !p.sign_in.message.is_empty() {
            sign_in.push(caption(&p.sign_in.message).into());
        }
        if !p.sign_in.url.is_empty() {
            let (on_open, url) = (hooks.on_open.clone(), p.sign_in.url.clone());
            sign_in.push(Button::new().style(ButtonStyle::Accent).horizontal_alignment(HorizontalAlignment::Left).content(TextBlock::new().text("Open Sign-In Page")).on_click(move || on_open.call(url.clone())).into());
        }
        if p.sign_in.takes_redirect {
            let key = format!("redirect:{id}");
            let typed = shown.state.drafts.get(&key).cloned().unwrap_or_default();
            let on_draft = hooks.on_draft.clone();
            let complete = tell(core, "Complete", Event::CompleteProviderSignIn { instance_id: id.clone(), callback_url: typed.trim().to_owned() }).grid_column(1).is_enabled(!typed.trim().is_empty());
            sign_in.push(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBox::new()
                            .automation_name(format!("Redirect URL for {name}"))
                            .placeholder_text("Signed in on another device? Paste the address the browser ended on")
                            .text(&typed)
                            .on_text_changed(move |text: Rc<str>| on_draft.call((key.clone(), text))),
                        complete,
                    ))
                    .into(),
            );
        }
        lines.push(Border::new().padding(Thickness::uniform(10.0)).corner_radius(6.0).background(theme::code()).content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(sign_in)).into());
    } else if !p.sign_in.message.is_empty() && p.sign_in.phase == "failed" {
        lines.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(&p.sign_in.message).into());
    }

    if p.install.busy || !p.install.message.is_empty() {
        let note = format!("{} {}", p.install.progress_label, p.install.message);
        if p.install.busy {
            let bar = ProgressBar::new().minimum(0.0).maximum(1000.0);
            lines.push(if p.install.progress == 0 { bar.is_indeterminate(true) } else { bar.value(f64::from(p.install.progress)) }.into());
        }
        if !note.trim().is_empty() {
            lines.push(caption(note.trim()).into());
        }
    }
    card(lines)
}

pub fn keybindings(core: &CoreLink, settings: &SettingsView) -> Vec<View> {
    let rows: Vec<View> = settings
        .keybindings
        .iter()
        .map(|binding| {
            let remove = Event::RemoveKeybinding { command: binding.command.clone(), shortcut: binding.shortcut.clone(), condition: binding.condition.clone() };
            Grid::new()
                .columns([GridLength::Star(3.0), GridLength::Auto, GridLength::Star(2.0), GridLength::Auto])
                .column_spacing(12.0)
                .children((
                    TextBlock::new().text(&binding.command).font_family(theme::MONO).font_size(theme::CAPTION).vertical_alignment(VerticalAlignment::Center).text_trimming(TextTrimming::CharacterEllipsis),
                    Border::new().grid_column(1).content(pill(&binding.shortcut, false)),
                    caption(&binding.condition).grid_column(2).vertical_alignment(VerticalAlignment::Center),
                    tell(core, "Remove", remove).grid_column(3).automation_name(format!("Remove the shortcut of {}", binding.command)),
                ))
                .into()
        })
        .collect();
    let mut out = vec![caption("The shortcuts saved on the server, which the web app and the desktop apps share.").into()];
    out.push(if rows.is_empty() { empty("No shortcuts are saved on the server.") } else { card(rows) });
    out
}

/// Where a task's runs go, in words.
fn destination(task: &ScheduledTaskView) -> String {
    match task.thread_mode.as_str() {
        "continue" | "fresh_session" if !task.thread_title.is_empty() => format!("in “{}”", task.thread_title),
        "continue" | "fresh_session" => "in its thread".to_owned(),
        _ => "a new thread each run".to_owned(),
    }
}

pub fn scheduled(core: &CoreLink, shown: &Shown<'_>, hooks: &Hooks<'_>) -> Vec<View> {
    let settings = shown.settings;
    let mut about = "A task sends a prompt on a schedule, or when its webhook is called.".to_owned();
    if !settings.scheduled_time_zone.is_empty() {
        about.push_str(&format!(" Times are in {}.", settings.scheduled_time_zone));
    }
    let mut out: Vec<View> = vec![caption(&about).into(), ask(hooks, "New Task…", "task-new:".to_owned()).horizontal_alignment(HorizontalAlignment::Left).into()];
    if settings.scheduled_tasks.is_empty() {
        out.push(empty("No scheduled tasks."));
    }
    for task in &settings.scheduled_tasks {
        let id = &task.id;
        let webhook = task.schedule_kind == "webhook";
        let enabled = {
            let (core, id, on) = (core.clone(), id.clone(), task.enabled);
            ToggleSwitch::new()
                .grid_column(1)
                .min_width(0.0)
                .is_on(on)
                .on_content(TextBlock::new())
                .off_content(TextBlock::new())
                .automation_name(format!("{} is enabled", task.name))
                .on_toggled(move |to: bool| {
                    if to != on {
                        core.send(Event::SetScheduledEnabled { id: id.clone(), enabled: to });
                    }
                })
        };
        let mut lines: Vec<View> = vec![
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((TextBlock::new().text(&task.name).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center), enabled))
                .into(),
            TextBlock::new().text(&task.prompt).text_wrapping(TextWrapping::WrapWholeWords).max_lines(3).text_trimming(TextTrimming::CharacterEllipsis).opacity(0.8).into(),
        ];
        let mut about = vec![task.schedule.clone(), task.project_title.clone(), destination(task)];
        if !task.next_run.is_empty() {
            about.push(format!("next {}", task.next_run));
        }
        if !task.last_run.is_empty() {
            about.push(format!("last {}", task.last_run));
        }
        about.retain(|part| !part.is_empty());
        lines.push(caption(&about.join(" · ")).into());
        if !task.run_state.is_empty() {
            lines.push(caption(&task.run_state).foreground(theme::accent()).opacity(1.0).into());
        } else if !task.last_error.is_empty() {
            lines.push(caption(&task.last_error).foreground(theme::critical()).opacity(1.0).into());
        }

        let runs_open = settings.scheduled_runs_task == *id;
        let mut actions: Vec<View> = Vec::new();
        if !webhook {
            actions.push(tell(core, "Run Now", Event::RunScheduledNow { id: id.clone() }).automation_name(format!("Run {} now", task.name)).into());
        }
        // The task's own thread, else the one its last run started.
        let thread = if task.thread_id.is_empty() { &task.last_thread_id } else { &task.thread_id };
        if !thread.is_empty() {
            actions.push(ask(hooks, "Open Thread", format!("thread-open:{thread}")).into());
        }
        actions.push(tell(core, if runs_open { "Hide Runs" } else { "Runs" }, Event::LoadScheduledRuns { id: if runs_open { String::new() } else { id.clone() } }).into());
        actions.push(ask(hooks, "Edit…", format!("task-edit:{id}")).automation_name(format!("Edit {}", task.name)).into());
        actions.push(ask(hooks, "Delete…", format!("task-delete:{id}")).automation_name(format!("Delete {}", task.name)).into());
        lines.push(buttons(actions));

        if webhook && !task.webhook_url.is_empty() {
            let (on_copy, url) = (hooks.on_copy.clone(), task.webhook_url.clone());
            let deliveries_open = settings.webhook_deliveries_task == *id;
            lines.push(TextBlock::new().text(&task.webhook_url).font_family(theme::MONO).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true).into());
            lines.push(buttons(vec![
                Button::new().content(TextBlock::new().text("Copy URL")).on_click(move || on_copy.call(url.clone())).into(),
                ask(hooks, "Rotate Token…", format!("task-rotate:{id}")).into(),
                tell(core, if deliveries_open { "Refresh Deliveries" } else { "Deliveries" }, Event::LoadWebhookDeliveries { id: id.clone() }).into(),
            ]));
            if deliveries_open {
                if settings.webhook_deliveries.is_empty() {
                    lines.push(caption("No deliveries yet.").into());
                }
                for delivery in &settings.webhook_deliveries {
                    let line = [delivery.received.as_str(), delivery.method.as_str(), delivery.outcome.as_str(), delivery.detail.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                    lines.push(caption(&line).into());
                }
            }
        }
        if runs_open {
            if settings.scheduled_runs.is_empty() {
                lines.push(caption("No runs yet.").into());
            }
            for run in &settings.scheduled_runs {
                let about = [run.trigger.as_str(), run.started.as_str(), run.thread_title.as_str(), run.error.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let mut row: Vec<View> = vec![
                    caption(&run.status).foreground(if run.failed { theme::critical() } else { theme::accent() }).opacity(1.0).vertical_alignment(VerticalAlignment::Center).into(),
                    caption(&about).grid_column(1).vertical_alignment(VerticalAlignment::Center).into(),
                ];
                if !run.thread_id.is_empty() {
                    row.push(ask(hooks, "Open", format!("thread-open:{}", run.thread_id)).grid_column(2).into());
                }
                lines.push(Grid::new().columns([GridLength::Pixel(150.0), GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children(row).into());
            }
        }
        out.push(card(lines));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tasks_destination_reads_as_words() {
        let mut task = ScheduledTaskView { thread_mode: "new_thread".into(), ..ScheduledTaskView::default() };
        assert_eq!(destination(&task), "a new thread each run");
        task.thread_mode = "continue".into();
        assert_eq!(destination(&task), "in its thread");
        task.thread_title = "Nightly check".into();
        assert_eq!(destination(&task), "in “Nightly check”");
    }
}
