//! The open thread: its transcript (messages, reasoning, commands, edits,
//! tool calls), the approvals and questions it waits on, and messages not
//! sent yet. Markdown is drawn block by block; code in a monospace font.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ItemView, MarkdownBlock, RequestView, SidebarView, ThreadPageView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, ProgressRing, ScrollBarVisibility, ScrollViewer,
    StackPanel, TextBlock, TextBox, TextWrapping, Thickness, View,
};

use crate::core_link::CoreLink;
use crate::theme;

/// Longest command output shown before the row says it was cut.
const OUTPUT_LINES: usize = 40;

pub fn thread(core: &CoreLink, page: &ThreadPageView) -> View {
    let mut rows: Vec<View> = vec![header(page)];
    if page.loading && page.items.is_empty() {
        rows.push(ProgressRing::new().is_active(true).width(24.0).height(24.0).margin(Thickness::uniform(24.0)).into());
    }
    rows.extend(page.items.iter().map(item));
    rows.extend(page.pending_sends.iter().map(|send| {
        bubble(true, vec![text(&send.text).into(), caption(if send.state == "waiting" { "Waiting for the connection" } else { "Sending…" }).into()])
    }));
    rows.extend(page.pending_requests.iter().map(|request| approval(core, request)));
    if page.working {
        rows.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .margin(Thickness::new(4.0, 8.0, 0.0, 0.0))
                .children(vec![ProgressRing::new().is_active(true).width(14.0).height(14.0).into(), caption("Working…").into()])
                .into(),
        );
    }
    // Stretched to the card's width (no max width: a centred column was
    // measured wider than the card and its right side cut off).
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).margin(Thickness::new(24.0, 12.0, 24.0, 20.0)).children(rows))
        .into()
}

/// What the route shows when no thread is open.
pub fn empty() -> View {
    TextBlock::new().text("Pick a thread on the left, or start a new one.").opacity(0.6).horizontal_alignment(HorizontalAlignment::Center).margin(Thickness::uniform(48.0)).into()
}

/// What adding a server folder as a project tells the window.
pub struct ProjectHooks {
    pub on_path: Callback<Rc<str>>,
    pub on_add: Callback<()>,
}

/// A new thread: where it runs (one of the server's projects, a scratch
/// project, or a folder on the server added now), then its first message
/// below. Nothing can be sent before a project is chosen.
pub fn new_thread(core: &CoreLink, sidebar: &SidebarView, path: &str, hooks: ProjectHooks) -> View {
    let selected = sidebar.selected_project_id.as_deref();
    let mut choices: Vec<View> = sidebar
        .projects
        .iter()
        .map(|project| {
            let (core, id) = (core.clone(), project.id.clone());
            let chosen = selected == Some(project.id.as_str());
            let label = if project.is_scratch { "Scratch".to_owned() } else { project.title.clone() };
            Button::new()
                .style(if chosen { ButtonStyle::Accent } else { ButtonStyle::Default })
                .automation_name(&label)
                .content(TextBlock::new().text(label))
                .on_click(move || core.send(Event::NewThread { project_id: Some(id.clone()) }))
                .into()
        })
        .collect();
    if !sidebar.projects.iter().any(|p| p.is_scratch) {
        let core = core.clone();
        choices.push(Button::new().automation_name("Scratch").content(TextBlock::new().text("Scratch")).on_click(move || core.send(Event::OpenScratch)).into());
    }
    let on_add = hooks.on_add;
    let lines: Vec<View> = vec![
        TextBlock::new().text("New thread").font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).into(),
        caption(if selected.is_some() { "Write its first message below." } else { "Choose where it runs, then write its first message below." }).into(),
        TextBlock::new().text("Project").font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 8.0, 0.0, 0.0)).into(),
        WrapRow(choices).into(),
        TextBlock::new().text("Or add a folder on the server").font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 8.0, 0.0, 0.0)).into(),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBox::new().automation_name("Server folder").placeholder_text(r"C:\src\my-project").text(path).on_text_changed(hooks.on_path),
                Button::new().grid_column(1).automation_name("Add project").is_enabled(!path.trim().is_empty()).content(TextBlock::new().text("Add")).on_click(move || on_add.call(())),
            ))
            .into(),
    ];
    StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).margin(Thickness::new(20.0, 16.0, 20.0, 20.0)).max_width(700.0).children(lines).into()
}

/// Buttons side by side (the projects); a long list scrolls sideways.
struct WrapRow(Vec<View>);

impl From<WrapRow> for View {
    fn from(row: WrapRow) -> View {
        ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(row.0))
            .into()
    }
}

fn header(page: &ThreadPageView) -> View {
    let mut details = vec![page.project_title.clone(), page.model_label.clone()];
    if let Some(branch) = &page.branch {
        details.push(branch.clone());
    }
    details.retain(|d| !d.is_empty());
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .margin(Thickness::new(0.0, 0.0, 0.0, 6.0))
        .children(vec![
            TextBlock::new().text(&page.title).font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::WrapWholeWords).into(),
            caption(&details.join(" · ")).into(),
        ])
        .into()
}

fn text(s: &str) -> TextBlock {
    TextBlock::new().text(s).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true)
}

fn caption(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_size(theme::CAPTION).opacity(0.65).text_wrapping(TextWrapping::WrapWholeWords)
}

fn mono(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_family(theme::MONO).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true)
}

/// A message: the user's on the right in a card, the agent's on the left.
fn bubble(mine: bool, children: Vec<View>) -> View {
    let content = StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(children);
    if mine {
        Border::new()
            .horizontal_alignment(HorizontalAlignment::Right)
            .max_width(640.0)
            .padding(Thickness::xy(12.0, 8.0))
            .background(theme::card())
            .border_brush(theme::stroke())
            .border_thickness(Thickness::uniform(1.0))
            .corner_radius(theme::RADIUS)
            .content(content)
            .into()
    } else {
        Border::new().content(content).into()
    }
}

/// A step the agent took (a command, an edit, a tool call), in a card.
fn step(icon: &str, title: &str, body: Vec<View>) -> View {
    let mut lines = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children(vec![theme::glyph(icon, 12.0).opacity(0.7).into(), TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::Wrap).into()])
            .into(),
    ];
    lines.extend(body);
    Border::new()
        .padding(Thickness::xy(12.0, 8.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).children(lines))
        .into()
}

fn item(item: &ItemView) -> View {
    match item.kind.as_str() {
        "user" => {
            let mut lines: Vec<View> = vec![text(&item.text).into()];
            if !item.attachments.is_empty() {
                lines.push(caption(&format!("Attached: {}", item.attachments.join(", "))).into());
            }
            bubble(true, lines)
        }
        "assistant" | "plan" => {
            let blocks: Vec<View> = if item.blocks.is_empty() { vec![text(&item.text).into()] } else { item.blocks.iter().map(block).collect() };
            if item.kind == "plan" { step("\u{E9D5}", if item.title.is_empty() { "Plan" } else { &item.title }, blocks) } else { bubble(false, blocks) }
        }
        // An aside: dimmer, as the other clients show it.
        "reasoning" => caption(&item.text).opacity(0.55).into(),
        "command" => {
            let mut body: Vec<View> = Vec::new();
            if !item.text.is_empty() {
                body.push(mono(&item.text).into());
            }
            if let Some(output) = item.output.as_deref().filter(|o| !o.trim().is_empty()) {
                let shown = last_lines(output, OUTPUT_LINES);
                body.push(Border::new().padding(Thickness::uniform(8.0)).background(theme::code()).corner_radius(4.0).content(mono(&shown)).into());
            }
            if let Some(code) = item.exit_code.filter(|c| *c != 0) {
                body.push(caption(&format!("Exited with {code}")).foreground(theme::critical()).into());
            }
            step("\u{E756}", if item.title.is_empty() { "Command" } else { &item.title }, body)
        }
        "file_change" => {
            let summary = item.diff.as_ref().map(|d| format!("{} (+{} −{})", d.path, d.additions, d.deletions)).unwrap_or_else(|| item.text.clone());
            step("\u{E70F}", if item.title.is_empty() { "Edited a file" } else { &item.title }, vec![mono(&summary).into()])
        }
        "error" => {
            let message = [item.text.as_str(), item.title.as_str()].into_iter().find(|s| !s.is_empty()).unwrap_or("The turn failed.");
            step("\u{E783}", "Error", vec![text(message).foreground(theme::critical()).into()])
        }
        "notice" | "checkpoint" => caption(if item.text.is_empty() { &item.title } else { &item.text }).into(),
        _ => {
            let mut body: Vec<View> = Vec::new();
            if !item.text.is_empty() {
                body.push(mono(&last_lines(&item.text, OUTPUT_LINES)).into());
            }
            step("\u{E90F}", if item.title.is_empty() { &item.kind } else { &item.title }, body)
        }
    }
}

fn block(b: &MarkdownBlock) -> View {
    let inline = || if b.runs.is_empty() { b.text.clone() } else { b.runs.iter().map(|r| r.text.as_str()).collect::<String>() };
    match b.kind.as_str() {
        "heading" => TextBlock::new()
            .text(inline())
            .font_size(match b.level {
                1 => 22.0,
                2 => 18.0,
                _ => 15.0,
            })
            .font_weight(FontWeight::SEMI_BOLD)
            .text_wrapping(TextWrapping::WrapWholeWords)
            .into(),
        "code" => Border::new().padding(Thickness::uniform(10.0)).background(theme::code()).corner_radius(6.0).content(mono(&b.text)).into(),
        "list_item" => {
            let marker = match b.task_checked {
                Some(true) => "☑ ".to_owned(),
                Some(false) => "☐ ".to_owned(),
                None if b.ordered => format!("{}. ", b.number),
                None => "• ".to_owned(),
            };
            text(&format!("{marker}{}", inline())).margin(Thickness::new(16.0 * f64::from(b.level.max(1)), 0.0, 0.0, 0.0)).into()
        }
        "quote" => Border::new()
            .padding(Thickness::new(10.0, 2.0, 0.0, 2.0))
            .border_brush(theme::stroke())
            .border_thickness(Thickness::new(3.0, 0.0, 0.0, 0.0))
            .content(text(&inline()).opacity(0.8))
            .into(),
        "rule" => Border::new().height(1.0).background(theme::stroke()).margin(Thickness::xy(0.0, 6.0)).into(),
        "table_row" => {
            let cells = if b.cell_runs.is_empty() { b.cells.clone() } else { b.cell_runs.iter().map(|runs| runs.iter().map(|r| r.text.as_str()).collect()).collect() };
            let row = text(&cells.join("  │  "));
            if b.header { row.font_weight(FontWeight::SEMI_BOLD).into() } else { row.into() }
        }
        _ => text(&inline()).into(),
    }
}

/// An approval or question the turn waits on, with its answers.
fn approval(core: &CoreLink, request: &RequestView) -> View {
    let answer = |label: &str, decision: &'static str, style: ButtonStyle| {
        let (core, id) = (core.clone(), request.id.clone());
        Button::new().style(style).content(TextBlock::new().text(label)).on_click(move || core.send(Event::Respond { request_id: id.clone(), decision: decision.to_owned() })).into()
    };
    let mut body: Vec<View> = vec![text(&request.prompt).into()];
    if let Some(detail) = request.detail.as_deref().filter(|d| !d.is_empty()) {
        body.push(Border::new().padding(Thickness::uniform(8.0)).background(theme::code()).corner_radius(4.0).content(mono(detail)).into());
    }
    if request.questions.is_empty() {
        let session = if request.session_label.is_empty() { "Allow for Session" } else { request.session_label.as_str() };
        body.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![
                    answer("Allow", "accept", ButtonStyle::Accent),
                    answer(session, "acceptForSession", ButtonStyle::Default),
                    answer("Decline", "decline", ButtonStyle::Default),
                ])
                .into(),
        );
    } else {
        let (core, id) = (core.clone(), request.id.clone());
        body.push(caption("Answering questions is not in this app yet: answer it from another Domovoi app, or skip it and the agent carries on.").into());
        body.push(Button::new().content(TextBlock::new().text("Skip")).on_click(move || core.send(Event::DismissUserInput { request_id: id.clone() })).into());
    }
    step("\u{E7BA}", if request.questions.is_empty() { "Approval needed" } else { "Question" }, body)
}

/// The last `n` lines of `s`, saying how many were left out.
fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.trim_end().to_owned();
    }
    format!("… {} lines above\n{}", lines.len() - n, lines[lines.len() - n..].join("\n"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_output_keeps_its_end() {
        let out: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let shown = super::last_lines(&out, 40);
        assert!(shown.starts_with("… 10 lines above\nline 11\n"));
        assert!(shown.ends_with("line 50"));
        assert_eq!(super::last_lines("a\nb\n", 40), "a\nb");
    }
}
