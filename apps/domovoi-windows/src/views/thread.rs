//! The open thread's page: its name and what can be done with it on top, the
//! find box, what the thread waits for elsewhere, then its rows (`items`),
//! what waits in it (`pending`) and, above the composer, the plan and
//! checklist the agent works through.
//!
//! A long thread draws its last entries (`transcript::WINDOW`) and more on
//! request: each row is drawn again whenever the thread changes, so the
//! count is what keeps a streaming reply quick.

use domovoi_core::Event;
use domovoi_core::view::{SidebarView, ThreadPageView, ThreadRowView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ElementRef, FontWeight, Grid, GridLength, HorizontalAlignment, Key, MenuExt as _, Orientation, ProgressBar, ProgressRing,
    ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::{CoreLink, now_ms};
use crate::store::Thread;
use crate::theme;
use crate::views::sidebar::{self, Icons};
use crate::views::transcript::{self, Act, Draw, duration};
use crate::views::{items, markdown, pending};

/// What the page tells the window, and where it scrolls.
pub struct Hooks<'a> {
    pub pending: &'a pending::Hooks,
    /// A choice from the thread's menu (`t-rename:<id>`, …).
    pub on_menu: Callback<Key>,
    /// The rows' scroll viewer, which the window keeps at its end.
    pub scroll: &'a ElementRef<ScrollViewer>,
}

fn caption(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_size(theme::CAPTION).opacity(0.65)
}

/// A small button with only a glyph on it.
fn icon_button(glyph: &str, name: &str) -> Button {
    Button::new().style(ButtonStyle::Subtle).automation_name(name).content(theme::glyph(glyph, 14.0))
}

/// A button that tells the core `event`.
fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION)).on_click(move || core.send(event.clone()))
}

/// A line that says something about the thread, with what can be done about it.
fn banner(glyph: &str, cells: Vec<View>) -> View {
    let mut all: Vec<View> = vec![theme::glyph(glyph, 13.0).foreground(theme::accent()).into()];
    all.extend(cells);
    Border::new()
        .padding(Thickness::xy(12.0, 6.0))
        .background(theme::code())
        .corner_radius(6.0)
        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(all))
        .into()
}

pub fn thread(core: &CoreLink, thread: &Thread, sidebar: &SidebarView, icons: Icons<'_>, waiting: &pending::State, draw: &Draw<'_>, hooks: Hooks<'_>) -> View {
    let page = &*thread.page;
    let mut top: Vec<View> = vec![header(core, page, sidebar, icons, draw, &hooks.on_menu)];
    if let Some(find) = &page.find {
        let count = if find.query.is_empty() {
            String::new()
        } else if find.total == 0 {
            "No matches".to_owned()
        } else {
            format!("{} of {}", find.active, find.total)
        };
        let on_act = draw.on_act.clone();
        let close = {
            let core = core.clone();
            icon_button("\u{E711}", "Close find").on_click(move || core.send(Event::CloseFind)).tooltip("Close (Esc)")
        };
        top.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(vec![
                    TextBox::new().automation_name("Text to find").placeholder_text("Find in thread").width(280.0).text(&draw.state.find).on_text_changed(move |text| on_act.call(Act::Find(text))).into(),
                    caption(&count).vertical_alignment(VerticalAlignment::Center).min_width(70.0).into(),
                    tell(core, "Previous", Event::FindNext { forward: false }).is_enabled(find.total > 0).into(),
                    tell(core, "Next", Event::FindNext { forward: true }).is_enabled(find.total > 0).tooltip("Next match (Ctrl+G)"),
                    close,
                ])
                .into(),
        );
    }
    // What the thread waits for elsewhere, its pull requests and issues, and a limit it ran into.
    for child in &page.waiting_children {
        top.push(banner("\u{E7BA}", vec![
            caption(if child.relation == "subagent" { "A subagent waits on you:" } else { "A thread waits on you:" }).vertical_alignment(VerticalAlignment::Center).into(),
            TextBlock::new().text(if child.title.is_empty() { "Untitled" } else { child.title.as_str() }).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into(),
            tell(core, "Open", Event::SelectThread { thread_id: child.thread_id.clone() }).into(),
        ]));
    }
    if !page.links.is_empty() {
        let chips = page.links.iter().map(|link| {
            let (on_act, url) = (draw.on_act.clone(), link.url.clone());
            let label = [link.label.as_str(), link.title.as_str(), link.state.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            Button::new()
                .content(TextBlock::new().text(label).font_size(theme::CAPTION).max_width(360.0).text_trimming(TextTrimming::CharacterEllipsis))
                .on_click(move || on_act.call(Act::Open(url.clone())))
                .tooltip(if link.kind == "pull_request" { "Open the pull request in the browser" } else { "Open the issue in the browser" })
        });
        top.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(chips.collect::<Vec<View>>()).into());
    }
    if !page.limit_resets.is_empty() {
        let mut cells: Vec<View> = vec![TextBlock::new().text(format!("Usage limit reached · resets {}", page.limit_resets)).vertical_alignment(VerticalAlignment::Center).into()];
        if page.limit_plan.is_empty() {
            cells.push(tell(core, "Resume at Reset", Event::PlanForLimitReset { snooze: false }).into());
            cells.push(tell(core, "Snooze Until Reset", Event::PlanForLimitReset { snooze: true }).into());
        } else {
            cells.push(caption(&format!("Scheduled: {}", page.limit_plan)).vertical_alignment(VerticalAlignment::Center).into());
            cells.push(tell(core, "Cancel", Event::CancelLimitPlan).into());
        }
        top.push(banner("\u{E823}", cells));
    }

    let mut rows: Vec<View> = Vec::new();
    let all = transcript::entries(&thread.rows);
    let (hidden, shown) = transcript::window(&all, draw.state.shown);
    if page.loading && thread.rows.is_empty() {
        rows.push(ProgressRing::new().is_active(true).width(24.0).height(24.0).margin(Thickness::uniform(24.0)).into());
    } else if thread.rows.is_empty() && page.pending_sends.is_empty() && page.queued.is_empty() && !page.working {
        rows.push(TextBlock::new().text("Ready to work. Ask the agent to look at files, write code or run commands.").opacity(0.6).horizontal_alignment(HorizontalAlignment::Center).margin(Thickness::uniform(40.0)).into());
    }
    if hidden > 0 {
        let on_act = draw.on_act.clone();
        rows.push(
            Button::new()
                .horizontal_alignment(HorizontalAlignment::Center)
                .content(TextBlock::new().text(format!("Show earlier messages ({hidden} more)")).font_size(theme::CAPTION))
                .on_click(move || on_act.call(Act::Earlier))
                .into(),
        );
    }
    rows.extend(shown.iter().map(|entry| items::entry(core, entry, draw)));
    if page.working || !page.background_tasks.is_empty() {
        rows.push(working(core, page, draw.on_act));
    }
    rows.extend(page.pending_sends.iter().map(|send| {
        let mut cells: Vec<View> = vec![
            TextBlock::new().text(&send.text).font_size(draw.size).text_wrapping(TextWrapping::WrapWholeWords).into(),
            caption(if send.state == "waiting" { "Waiting for the connection" } else { "Sending…" }).into(),
        ];
        if send.state == "waiting" {
            // Not gone yet: it can come back into the composer.
            cells.push(tell(core, "Take Back", Event::CancelSend { id: send.id.clone() }).horizontal_alignment(HorizontalAlignment::Right).into());
        }
        Border::new()
            .horizontal_alignment(HorizontalAlignment::Right)
            .max_width(680.0)
            .padding(Thickness::xy(14.0, 10.0))
            .background(theme::card())
            .border_brush(theme::stroke())
            .border_thickness(Thickness::uniform(1.0))
            .corner_radius(12.0)
            .opacity(0.75)
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(cells))
            .into()
    }));
    if !page.queued.is_empty() {
        rows.push(pending::queued(core, page, waiting, hooks.pending));
    }
    rows.extend(page.secret_requests.iter().map(|asked| pending::secret(core, asked, waiting, hooks.pending)));
    rows.extend(page.pending_requests.iter().map(|request| pending::request(core, request, waiting, hooks.pending)));

    // Stretched to the card's width (no max width: a centred column was
    // measured wider than the card and its right side cut off).
    let mut layers: Vec<View> = vec![
        ScrollViewer::new()
            .element_ref(hooks.scroll)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).margin(Thickness::new(24.0, 8.0, 24.0, 20.0)).children(rows))
            .into(),
    ];
    if !draw.state.following {
        let on_act = draw.on_act.clone();
        let label = if draw.state.unseen { "New messages" } else { "Jump to latest" };
        layers.push(
            Button::new()
                .style(if draw.state.unseen { ButtonStyle::Accent } else { ButtonStyle::Default })
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::uniform(12.0))
                .automation_name(label)
                .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(vec![theme::glyph("\u{E74B}", 11.0).into(), TextBlock::new().text(label).font_size(theme::CAPTION).into()]))
                .on_click(move || on_act.call(Act::Latest))
                .into(),
        );
    }
    let mut parts: Vec<View> = vec![
        StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).margin(Thickness::new(24.0, 12.0, 16.0, 6.0)).children(top).into(),
        Grid::new().grid_row(1).children(layers).into(),
    ];
    if page.plan.is_some() || page.tasks.is_some() {
        parts.push(Border::new().grid_row(2).content(progress(core, page, draw)).into());
    }
    Grid::new().rows([GridLength::Auto, GridLength::STAR, GridLength::Auto]).children(parts).into()
}

/// What the route shows when no thread is open.
pub fn empty() -> View {
    TextBlock::new().text("Pick a thread on the left, or start a new one.").opacity(0.6).horizontal_alignment(HorizontalAlignment::Center).margin(Thickness::uniform(48.0)).into()
}

/// The thread's name, where it runs and with what; then what can be done with it.
fn header(core: &CoreLink, page: &ThreadPageView, sidebar: &SidebarView, icons: Icons<'_>, draw: &Draw<'_>, on_menu: &Callback<Key>) -> View {
    let mut details = vec![page.project_title.clone(), page.model_label.clone()];
    details.extend(page.branch.iter().map(|branch| format!("⑂ {branch}")));
    details.retain(|d| !d.is_empty());
    let project = sidebar.projects.iter().find(|p| p.id == page.project_id);
    let mut under: Vec<View> =
        [project.map(|p| icons.tile(&p.icon, 16.0)), Some(caption(&details.join(" · ")).vertical_alignment(VerticalAlignment::Center).tooltip(if page.cwd.is_empty() { "Where the thread works is not known yet".to_owned() } else { format!("Works in {}", page.cwd) }))]
            .into_iter()
            .flatten()
            .collect();
    if let Some(parent) = &page.parent {
        let label = format!("↑ {}", if parent.title.is_empty() { "the thread it came from" } else { parent.title.as_str() });
        under.push(
            tell(core, &label, Event::SelectThread { thread_id: parent.thread_id.clone() })
                .style(ButtonStyle::Subtle)
                .tooltip(if parent.relation == "subagent" { "The thread this subagent works for" } else { "The thread this one was forked from" }),
        );
    }

    let mut right: Vec<View> = Vec::new();
    if page.can_merge_back {
        right.push(tell(core, "Merge Back", Event::MergeBack).tooltip("Send this fork's work back to the thread it came from"));
    }
    if let Some(meter) = usage(core, page) {
        right.push(meter);
    }
    if page.can_fork {
        let (core, id) = (core.clone(), page.id.clone());
        right.push(Button::new().style(ButtonStyle::Subtle).automation_name("Fork thread").content(crate::widgets::line_icon("git-branch", 16.0, icons.dark)).on_click(move || core.send(Event::ForkThread { thread_id: id.clone() })).tooltip("Fork: a new thread with the conversation so far"));
    }
    {
        let (core, query) = (core.clone(), draw.state.find.clone());
        right.push(icon_button("\u{E721}", "Find in thread").on_click(move || core.send(Event::FindInThread { query: query.clone() })).tooltip("Find in thread (Ctrl+F)"));
    }
    {
        let core = core.clone();
        right.push(icon_button("\u{E72C}", "Reload thread").on_click(move || core.send(Event::ReloadThread)).tooltip("Load the thread again"));
    }
    if page.artifact_count > 0 {
        right.push(tell(core, &format!("Artifacts {}", page.artifact_count), Event::ShowPanel { tab: "artifacts".into() }).style(ButtonStyle::Subtle).tooltip("What this thread made (Ctrl+Shift+A)"));
    }
    {
        let core = core.clone();
        right.push(icon_button("\u{E8A0}", "Side panel").on_click(move || core.send(Event::TogglePanel)).tooltip("The diff, artifacts, terminal, files and Git beside the thread (Ctrl+J)"));
    }
    // The thread's menu, as the list has it; a thread the list does not show still has one.
    let listed = sidebar::find_row(sidebar, &page.id).cloned();
    let row = listed.unwrap_or_else(|| ThreadRowView { id: page.id.clone(), title: page.title.clone(), pinned: page.pinned, settled: page.settled, archived: page.archived, ..ThreadRowView::default() });
    right.push(icon_button("\u{E712}", "Thread actions").menu(sidebar::thread_menu(&row, Some(page), on_menu)));

    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(12.0)
        .children((
            StackPanel::new().orientation(Orientation::Vertical).spacing(2.0).children(vec![
                TextBlock::new().text(if page.title.is_empty() { "New thread" } else { page.title.as_str() }).font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
                StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(under).into(),
            ]),
            StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(2.0).vertical_alignment(VerticalAlignment::Top).children(right),
        ))
        .into()
}

/// How full the thread's context is (or, without that, its fullest usage
/// window); the figures behind it on hover, and Compact when the provider has it.
fn usage(core: &CoreLink, page: &ThreadPageView) -> Option<View> {
    let session = page.usage.as_ref();
    let context = page.context.as_ref();
    let fullest = session.and_then(|u| u.fullest.as_ref());
    let (label, alarming) = match (context, fullest) {
        (Some(context), _) if context.max_tokens > 0 => (format!("Context {}%", context.percent), context.high || context.critical),
        (Some(context), _) => (context.label.clone(), false),
        (None, Some(window)) => (format!("{} {}%", window.label, window.percent), window.high),
        (None, None) => return None,
    };
    let mut tip: Vec<String> = Vec::new();
    if let Some(context) = context {
        tip.push(context.label.clone());
        if (1..100).contains(&context.compact_at_percent) {
            tip.push(format!("Compacts on its own at {}%", context.compact_at_percent));
        }
        tip.extend([&context.remaining_label, &context.cost].into_iter().filter(|s| !s.is_empty()).cloned());
        if !context.cache_hit_label.is_empty() {
            tip.push(format!("Cache hits {}", context.cache_hit_label));
        }
        tip.extend(context.breakdown.iter().map(|line| format!("{} {}", line.label, line.value)));
    }
    if let Some(limits) = session.and_then(|u| u.limits.as_ref()) {
        tip.extend(limits.windows.iter().map(|w| {
            let details = crate::widgets::limit_details(w);
            if details.is_empty() { format!("{} {}%", w.label, w.percent) } else { format!("{} {}% · {details}", w.label, w.percent) }
        }));
    }
    if let Some(notice) = session.map(|u| u.limits_notice.as_str()).filter(|n| !n.is_empty()) {
        tip.push(notice.to_owned());
    }
    if let Some(checked) = session.map(|u| u.limits_checked.as_str()).filter(|c| !c.is_empty()) {
        tip.push(if checked == "now" { "Limits read just now".to_owned() } else { format!("Limits read {checked} ago") });
    }
    // An account window that is nearly used up shows on the meter even when the context is not.
    let alarming = alarming || session.is_some_and(|u| u.limit_alert == "danger");
    let text = TextBlock::new().text(label).font_size(theme::CAPTION);
    let chip = Border::new()
        .padding(Thickness::xy(8.0, 3.0))
        .corner_radius(10.0)
        .background(theme::code())
        .vertical_alignment(VerticalAlignment::Center)
        .content(if alarming { text.foreground(theme::critical()) } else { text.opacity(0.8) })
        .tooltip(tip.join("\n"));
    let mut cells = vec![chip];
    if context.is_some_and(|c| c.can_compact) {
        cells.push(tell(core, "Compact", Event::Compact).style(ButtonStyle::Subtle).tooltip("Have the agent compact its context now"));
    }
    Some(StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).margin(Thickness::new(0.0, 0.0, 6.0, 0.0)).children(cells).into())
}

/// That the agent is at work and for how long, and what runs in the background.
fn working(core: &CoreLink, page: &ThreadPageView, on_act: &Callback<Act>) -> View {
    let mut lines: Vec<View> = Vec::new();
    if page.working {
        let doing = match page.working_activity.as_ref().and_then(|a| a.kind.as_deref()) {
            Some("starting") => "Starting",
            Some("reasoning") => "Thinking",
            Some("command") => "Running a command",
            Some("file_change") => "Editing",
            _ => "Working",
        };
        let since = if page.working_since_ms > 0 { format!(" · {}", duration(now_ms() - page.working_since_ms)) } else { String::new() };
        let mut cells: Vec<View> = vec![ProgressRing::new().is_active(true).width(14.0).height(14.0).into(), caption(&format!("{doing}…{since}")).vertical_alignment(VerticalAlignment::Center).into()];
        // The step under way may be folded into its group.
        if let Some(step) = page.working_activity.as_ref().and_then(|a| a.step_item_id.clone()) {
            let on_act = on_act.clone();
            cells.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content(TextBlock::new().text("Show the Step").font_size(theme::CAPTION))
                    .on_click(move || on_act.call(Act::Reveal(step.clone())))
                    .tooltip("Opens the steps of the turn under way"),
            );
        }
        lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(cells).into());
    }
    for task in &page.background_tasks {
        let about = [task.status.as_str(), task.progress.as_deref().unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let mut cells: Vec<View> = vec![
            theme::glyph(if task.kind == "subagent" { "\u{E99A}" } else { "\u{E756}" }, 12.0).opacity(0.7).into(),
            TextBlock::new().text(&task.label).font_size(theme::CAPTION + 1.0).vertical_alignment(VerticalAlignment::Center).text_trimming(TextTrimming::CharacterEllipsis).max_width(420.0).into(),
            caption(&about).vertical_alignment(VerticalAlignment::Center).into(),
        ];
        if let Some(child) = &task.child_thread_id {
            cells.push(tell(core, "Open", Event::SelectThread { thread_id: child.clone() }).into());
        }
        lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(cells).into());
    }
    StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).margin(Thickness::new(4.0, 4.0, 0.0, 0.0)).children(lines).into()
}

/// The latest plan and checklist at a glance ("Plan · Tasks 2/5 · Write the
/// tests"); opened, the plan's text, the to-dos and Implement.
fn progress(core: &CoreLink, page: &ThreadPageView, draw: &Draw<'_>) -> View {
    let open = draw.state.progress_open;
    let label = [page.plan.as_ref().map(|p| p.label.as_str()), page.tasks.as_ref().map(|t| t.label.as_str())].into_iter().flatten().filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" · ");
    let mut strip: Vec<View> = vec![
        theme::glyph("\u{E9D5}", 13.0).foreground(theme::accent()).into(),
        TextBlock::new().grid_column(1).text(if label.is_empty() { "Plan and checklist" } else { label.as_str() }).font_size(theme::CAPTION + 1.0).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
    ];
    if let Some(tasks) = page.tasks.as_ref().filter(|t| t.total > 0) {
        strip.push(ProgressBar::new().grid_column(2).width(72.0).minimum(0.0).maximum(f64::from(tasks.total)).value(f64::from(tasks.done)).vertical_alignment(VerticalAlignment::Center).into());
    }
    strip.push(theme::glyph(if open { "\u{E70D}" } else { "\u{E70E}" }, 10.0).grid_column(3).opacity(0.6).into());
    let on_act = draw.on_act.clone();
    let strip = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(if open { "Hide the plan and checklist" } else { "Show the plan and checklist" })
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(8.0).children(strip))
        .on_click(move || on_act.call(Act::Progress));
    let mut parts: Vec<View> = Vec::new();
    if open {
        let mut details: Vec<View> = Vec::new();
        if let Some(plan) = &page.plan {
            let mut head: Vec<View> = vec![TextBlock::new().text(if plan.label.is_empty() { "Plan" } else { plan.label.as_str() }).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into()];
            if plan.can_implement && !plan.plan_id.is_empty() {
                head.push(tell(core, "Implement Plan", Event::ImplementPlan { plan_id: plan.plan_id.clone() }).style(ButtonStyle::Accent).into());
            }
            details.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(10.0).children(head).into());
            details.extend(markdown::blocks(&plan.blocks, draw));
        }
        if let Some(tasks) = &page.tasks {
            let mut head = format!("Checklist · {}/{}", tasks.done, tasks.total);
            if tasks.previous_run {
                // The list is from an earlier turn, not the one under way.
                head.push_str(" · from the previous run");
            }
            details.push(TextBlock::new().text(head).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 4.0, 0.0, 0.0)).into());
            if let Some(explanation) = tasks.explanation.as_deref().filter(|e| !e.is_empty()) {
                details.push(caption(explanation).text_wrapping(TextWrapping::WrapWholeWords).into());
            }
            details.push(items::todos(&tasks.todos));
        }
        parts.push(
            ScrollViewer::new()
                .max_height(320.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).margin(Thickness::new(12.0, 10.0, 16.0, 6.0)).children(details))
                .into(),
        );
    }
    parts.push(strip.into());
    Border::new()
        .margin(Thickness::new(theme::GAP * 2.0, 0.0, theme::GAP * 2.0, 6.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).children(parts))
        .into()
}
