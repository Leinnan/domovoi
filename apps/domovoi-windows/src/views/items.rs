//! A thread's rows: what was said, what the agent thought, and each step it
//! took (a command, an edit, a search, a subagent), with the plans, to-do
//! lists and checkpoints between them.
//!
//! A step is one line (its verb, what it worked on, how it went); opened, it
//! shows its arguments, output, change and links. The steps of one turn fold
//! under a summary line.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ImageView, ItemView, TodoView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Orientation, ProgressRing, StackPanel, Stretch, TextBlock, TextTrimming,
    TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::views::markdown;
use crate::views::transcript::{Act, Draw, Entry, duration};
use crate::{theme, time, widgets};

/// The most of a step's output drawn at once.
const OUTPUT_LINES: usize = 60;

fn text(s: &str, size: f64) -> TextBlock {
    TextBlock::new().text(s).font_size(size).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true)
}

fn caption(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_size(theme::CAPTION).opacity(0.65).text_wrapping(TextWrapping::WrapWholeWords)
}

fn mono(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_family(theme::MONO).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true)
}

/// A small button with only a glyph on it, which tells the page `act`.
fn act(glyph: &str, name: &str, draw: &Draw<'_>, act: Act) -> View {
    let on_act = draw.on_act.clone();
    Button::new().style(ButtonStyle::Subtle).automation_name(name).content(theme::glyph(glyph, 12.0).opacity(0.75)).on_click(move || on_act.call(act.clone())).tooltip(name)
}

/// A button that tells the core `event`.
fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION)).on_click(move || core.send(event.clone()))
}

fn card(lines: Vec<View>) -> Border {
    Border::new()
        .padding(Thickness::xy(12.0, 8.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(lines))
}

/// A word in a pill at a line's end.
fn pill(label: &str, failed: bool) -> View {
    let label = TextBlock::new().text(label).font_size(theme::CAPTION);
    Border::new()
        .padding(Thickness::xy(7.0, 1.0))
        .corner_radius(9.0)
        .background(theme::code())
        .vertical_alignment(VerticalAlignment::Center)
        .content(if failed { label.foreground(theme::critical()) } else { label.opacity(0.75) })
        .into()
}

/// When a finished turn started, and how long it took: `14:02 · 1m 12s`.
fn turn_footer(item: &ItemView) -> View {
    let took = duration(item.run_duration_ms);
    let label = match time::local_clock(item.run_started_ms) {
        Some(started) => format!("{started} · {took}"),
        None => took,
    };
    caption(&label).opacity(0.5).vertical_alignment(VerticalAlignment::Center).into()
}

/// Pictures as thumbnails; pressed, one opens in the viewer.
pub fn pictures(images: &[ImageView], side: f64, draw: &Draw<'_>) -> View {
    let thumbnails = images.iter().enumerate().map(|(at, image)| {
        let content: View = match draw.media.image(&image.url) {
            Some(picture) => Image::new().source_data(picture).stretch(Stretch::UniformToFill).width(side).height(side * 0.75).into(),
            None => Border::new().width(side).height(side * 0.75).background(theme::code()).content(theme::glyph("\u{EB9F}", 20.0).opacity(0.4).horizontal_alignment(HorizontalAlignment::Center)).into(),
        };
        let (on_act, all) = (draw.on_act.clone(), images.to_vec());
        Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name(if image.name.is_empty() { "Picture" } else { image.name.as_str() })
            .content(Border::new().corner_radius(6.0).content(content))
            .on_click(move || on_act.call(Act::View(all.clone(), at)))
            .tooltip(if image.name.is_empty() { "Open the picture" } else { image.name.as_str() })
    });
    StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(thumbnails.collect::<Vec<View>>()).into()
}

/// A to-do list: done, under way and waiting steps.
pub fn todos(todos: &[TodoView]) -> View {
    let rows = todos.iter().map(|todo| {
        let (glyph, brush, dim) = match todo.status.as_str() {
            "completed" => ("\u{E73E}", theme::accent(), 0.6),
            "in_progress" => ("\u{E916}", theme::accent(), 1.0),
            _ => ("\u{EA3A}", theme::primary(), 0.85),
        };
        Grid::new()
            .columns([GridLength::Pixel(20.0), GridLength::STAR])
            .children((
                theme::glyph(glyph, 11.0).foreground(brush).opacity(dim).vertical_alignment(VerticalAlignment::Top).margin(Thickness::new(0.0, 4.0, 0.0, 0.0)),
                TextBlock::new().grid_column(1).text(&todo.text).opacity(dim).text_wrapping(TextWrapping::WrapWholeWords),
            ))
            .into()
    });
    StackPanel::new().orientation(Orientation::Vertical).spacing(3.0).children(rows.collect::<Vec<View>>()).into()
}

/// The glyph for a kind of step (`ItemView::icon`).
fn step_glyph(icon: &str) -> &'static str {
    match icon {
        "read" => "\u{E8A5}",
        "image" => "\u{EB9F}",
        "search" => "\u{E721}",
        "list" => "\u{E8FD}",
        "command" => "\u{E756}",
        "edit" => "\u{E70F}",
        "web" | "fetch" => "\u{E774}",
        "skill" => "\u{E945}",
        "agent" => "\u{E99A}",
        "question" => "\u{E897}",
        "wait" => "\u{E823}",
        _ => "\u{E90F}",
    }
}

/// The end of `s`: its last `n` lines, saying how many came before.
fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.trim_end().to_owned();
    }
    format!("… {} lines above\n{}", lines.len() - n, lines[lines.len() - n..].join("\n"))
}

/// A step the agent took: one line, and under it (opened) what it used and made.
fn step(core: &CoreLink, item: &ItemView, draw: &Draw<'_>) -> View {
    let open = item.expandable && draw.state.open(&item.id, !item.collapsed);
    let brush = match item.status.as_str() {
        "failed" => theme::critical(),
        "running" => theme::accent(),
        _ => theme::primary(),
    };
    let mut head: Vec<View> = Vec::new();
    if item.expandable {
        head.push(theme::glyph(if open { "\u{E70D}" } else { "\u{E76C}" }, 9.0).opacity(0.6).into());
    }
    head.push(if item.status == "running" { ProgressRing::new().is_active(true).width(12.0).height(12.0).into() } else { theme::glyph(step_glyph(&item.icon), 12.0).foreground(brush).opacity(0.85).into() });
    if !item.verb.is_empty() {
        head.push(TextBlock::new().text(&item.verb).font_size(theme::CAPTION + 1.0).opacity(0.7).vertical_alignment(VerticalAlignment::Center).into());
    }
    let title = if item.title.is_empty() && item.verb.is_empty() { item.kind.as_str() } else { item.title.as_str() };
    let title = TextBlock::new().text(title).font_size(theme::CAPTION + 1.0).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center);
    head.push(if item.mono { title.font_family(theme::MONO).into() } else { title.font_weight(FontWeight::SEMI_BOLD).into() });
    if !item.subtitle.is_empty() {
        head.push(caption(&item.subtitle).text_wrapping(TextWrapping::NoWrap).vertical_alignment(VerticalAlignment::Center).into());
    }
    let mut tail: Vec<View> = Vec::new();
    if !item.badge.is_empty() {
        tail.push(pill(&item.badge, item.badge_tone == "failed"));
    }
    // A failed exit is in the badge ("exit 101"); a clean one gets a quiet check.
    if item.kind == "command" && item.status != "running" && item.exit_code == Some(0) {
        tail.push(theme::glyph("\u{E73E}", 11.0).foreground(theme::accent()).tooltip("Exited with code 0"));
    }
    let line = Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .children((
            StackPanel::new().orientation(Orientation::Horizontal).spacing(7.0).children(head),
            StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(6.0).children(tail),
        ));
    let line: View = if item.expandable {
        let (on_act, id) = (draw.on_act.clone(), item.id.clone());
        Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(format!("{} {}", item.verb, item.title))
            .content(line)
            .on_click(move || on_act.call(Act::Toggle(id.clone())))
            .into()
    } else {
        Border::new().padding(Thickness::xy(11.0, 5.0)).content(line).into()
    };

    let mut rows: Vec<View> = vec![line];
    // What the agent looked at shows without opening the step.
    if !item.images.is_empty() {
        rows.push(Border::new().padding(Thickness::new(30.0, 0.0, 0.0, 4.0)).content(pictures(&item.images, 120.0, draw)).into());
    }
    if open {
        let mut details: Vec<View> = Vec::new();
        for field in item.fields.iter() {
            let value = TextBlock::new().grid_column(1).text(&field.value).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true);
            details.push(
                Grid::new()
                    .columns([GridLength::Pixel(110.0), GridLength::STAR])
                    .children((caption(&field.label), if field.mono { value.font_family(theme::MONO) } else { value }))
                    .into(),
            );
        }
        // A step with no line of its own for its input (an unknown tool) shows it here.
        if item.fields.is_empty() && !item.text.is_empty() && item.kind != "command" {
            details.push(mono(&last_lines(&item.text, OUTPUT_LINES)).into());
        }
        if let Some(output) = item.output.as_deref().filter(|o| !o.trim().is_empty()) {
            details.push(Border::new().padding(Thickness::uniform(8.0)).background(theme::code()).corner_radius(4.0).content(mono(&last_lines(output, OUTPUT_LINES))).into());
        }
        if item.truncated {
            details.push(tell(core, "Show Full Output", Event::ExpandItem { id: item.id.clone() }).horizontal_alignment(HorizontalAlignment::Left).into());
        }
        if let Some(diff) = &item.diff {
            details.push(widgets::diff_file(diff, draw.dark, 80));
        }
        for link in item.links.iter() {
            let (on_act, url) = (draw.on_act.clone(), link.url.clone());
            let cells: Vec<View> = vec![
                theme::glyph("\u{E8A7}", 11.0).foreground(theme::accent()).into(),
                TextBlock::new().text(&link.title).font_size(theme::CAPTION + 1.0).foreground(theme::accent()).text_trimming(TextTrimming::CharacterEllipsis).max_width(520.0).into(),
                caption(&link.host).vertical_alignment(VerticalAlignment::Center).into(),
            ];
            details.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .automation_name(&link.title)
                    .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(cells))
                    .on_click(move || on_act.call(Act::Open(url.clone())))
                    .tooltip(if link.snippet.is_empty() { link.url.as_str() } else { link.snippet.as_str() }),
            );
        }
        if !details.is_empty() {
            rows.push(Border::new().padding(Thickness::new(30.0, 0.0, 8.0, 6.0)).content(StackPanel::new().orientation(Orientation::Vertical).spacing(5.0).children(details)).into());
        }
    }
    // Web pages an edit wrote open as artifacts.
    if item.kind == "file_change" && !item.streaming {
        let pages: Vec<&String> = item.artifact_paths.iter().filter(|path| path.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))).collect();
        for path in &pages {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
            let (on_act, open) = (draw.on_act.clone(), (*path).clone());
            rows.push(
                Button::new()
                    .margin(Thickness::new(30.0, 0.0, 0.0, 4.0))
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .content(TextBlock::new().text(if pages.len() == 1 { "Open Preview".to_owned() } else { format!("Open {name}") }).font_size(theme::CAPTION))
                    .on_click(move || on_act.call(Act::Preview(open.clone())))
                    .tooltip(path.as_str()),
            );
        }
    }
    // Something the step made to look at: its page on the web, where it has one.
    if let Some(url) = item.artifact_hosted_url.clone().filter(|_| item.artifact_id.is_some()) {
        let on_act = draw.on_act.clone();
        rows.push(
            Button::new()
                .margin(Thickness::new(30.0, 0.0, 0.0, 4.0))
                .horizontal_alignment(HorizontalAlignment::Left)
                .content(TextBlock::new().text(format!("Open {}", if item.artifact_title.is_empty() { "the artifact" } else { item.artifact_title.as_str() })).font_size(theme::CAPTION))
                .on_click(move || on_act.call(Act::Open(url.clone())))
                .into(),
        );
    }
    StackPanel::new().orientation(Orientation::Vertical).children(rows).into()
}

/// One row of the thread.
fn row(core: &CoreLink, item: &ItemView, draw: &Draw<'_>) -> View {
    let size = draw.size;
    match item.kind.as_str() {
        "user" => {
            let mut lines: Vec<View> = Vec::new();
            if let Some(issue) = &item.issue_card {
                lines.push(act("\u{E71B}", &issue.issue_label, draw, Act::Open(issue.issue_url.clone())));
            }
            if !item.text.is_empty() {
                lines.push(text(&item.text, size).into());
            }
            if !item.images.is_empty() {
                lines.push(pictures(&item.images, 200.0, draw));
            } else if !item.attachments.is_empty() {
                lines.push(caption(&format!("Attached: {}", item.attachments.join(", "))).into());
            }
            let edit = {
                let (core, text) = (core.clone(), item.text.clone());
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .automation_name("Edit and send again")
                    .content(theme::glyph("\u{E70F}", 12.0).opacity(0.75))
                    .on_click(move || core.send(Event::EditMessage { text: text.clone() }))
                    .tooltip("Edit and send again")
            };
            lines.push(StackPanel::new().orientation(Orientation::Horizontal).horizontal_alignment(HorizontalAlignment::Right).children(vec![act("\u{E8C8}", "Copy the message", draw, Act::Copy(item.text.clone())), edit]).into());
            Border::new()
                .horizontal_alignment(HorizontalAlignment::Right)
                .max_width(680.0)
                .padding(Thickness::new(14.0, 10.0, 8.0, 2.0))
                .background(theme::tint(palette::project_rgb("blue", draw.dark), draw.dark))
                .border_brush(theme::stroke())
                .border_thickness(Thickness::uniform(1.0))
                .corner_radius(12.0)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(lines))
                .into()
        }
        "assistant" => {
            let mut lines: Vec<View> = if item.blocks.is_empty() { vec![text(&item.text, size).into()] } else { markdown::blocks(&item.blocks, draw) };
            if item.streaming {
                lines.push(TextBlock::new().text("▍").font_size(size).foreground(theme::accent()).into());
            } else if item.can_fork_here || item.run_duration_ms > 0 {
                let mut foot: Vec<View> = Vec::new();
                if item.run_duration_ms > 0 {
                    foot.push(turn_footer(item));
                }
                if !item.text.is_empty() {
                    foot.push(act("\u{E8C8}", "Copy the reply", draw, Act::Copy(item.text.clone())));
                }
                if item.can_fork_here {
                    foot.push(tell(core, "Fork Here", Event::ForkFromItem { item_id: item.id.clone() }).style(ButtonStyle::Subtle).tooltip("A new thread with the conversation up to here"));
                }
                lines.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(foot).into());
            }
            StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines).into()
        }
        "reasoning" => {
            let open = draw.state.open(&item.id, !item.collapsed);
            let (on_act, id) = (draw.on_act.clone(), item.id.clone());
            let mut lines: Vec<View> = vec![
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .automation_name("Reasoning")
                    .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(7.0).children(vec![
                        theme::glyph(if open { "\u{E70D}" } else { "\u{E76C}" }, 9.0).opacity(0.6).into(),
                        theme::glyph("\u{EA80}", 12.0).opacity(0.7).into(),
                        TextBlock::new().text(if item.title.is_empty() { "Reasoning" } else { item.title.as_str() }).font_size(theme::CAPTION + 1.0).opacity(0.7).into(),
                    ]))
                    .on_click(move || on_act.call(Act::Toggle(id.clone())))
                    .into(),
            ];
            if open && !item.text.is_empty() {
                lines.push(caption(&item.text).opacity(0.6).margin(Thickness::new(30.0, 0.0, 0.0, 4.0)).is_text_selection_enabled(true).into());
            }
            StackPanel::new().orientation(Orientation::Vertical).children(lines).into()
        }
        "plan" => {
            let mut buttons: Vec<View> = Vec::new();
            if item.superseded {
                buttons.push(pill("Superseded", false));
            }
            if item.can_implement && !item.plan_id.is_empty() {
                buttons.push(tell(core, "Implement Plan", Event::ImplementPlan { plan_id: item.plan_id.clone() }).style(ButtonStyle::Accent).into());
            }
            let (on_act, id) = (draw.on_act.clone(), item.id.clone());
            buttons.push(Button::new().content(TextBlock::new().text("Save…").font_size(theme::CAPTION)).on_click(move || on_act.call(Act::SavePlan(id.clone()))).tooltip("Save the plan as a file in the workspace"));
            let mut lines: Vec<View> = vec![
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                            theme::glyph("\u{E9D5}", 14.0).foreground(theme::accent()).into(),
                            TextBlock::new().text(if item.title.is_empty() { "Proposed Plan" } else { item.title.as_str() }).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into(),
                        ]),
                        StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(6.0).children(buttons),
                    ))
                    .into(),
            ];
            if item.blocks.is_empty() {
                lines.push(text(&item.text, size).into());
            } else {
                lines.extend(markdown::blocks(&item.blocks, draw));
            }
            card(lines).border_brush(theme::accent()).opacity(if item.superseded { 0.6 } else { 1.0 }).into()
        }
        "todo" => card(vec![
            StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                theme::glyph("\u{E9D5}", 12.0).opacity(0.8).into(),
                TextBlock::new().text(if item.title.is_empty() { "Tasks" } else { item.title.as_str() }).font_weight(FontWeight::SEMI_BOLD).font_size(theme::CAPTION + 1.0).into(),
            ]).into(),
            todos(&item.todos),
        ])
        .into(),
        "checkpoint" => {
            let Some(checkpoint) = &item.checkpoint else {
                return caption(if item.text.is_empty() { &item.title } else { &item.text }).into();
            };
            let (added, removed) = (theme::rgb(palette::project_rgb("green", draw.dark)), theme::rgb(palette::project_rgb("red", draw.dark)));
            let mut counts: Vec<View> = Vec::new();
            // A turn that changed nothing has no counts to show.
            if checkpoint.files_changed > 0 {
                counts.push(caption(&format!("{} file{}", checkpoint.files_changed, if checkpoint.files_changed == 1 { "" } else { "s" })).vertical_alignment(VerticalAlignment::Center).into());
                counts.push(caption(&format!("+{}", checkpoint.additions)).foreground(added.clone()).opacity(1.0).vertical_alignment(VerticalAlignment::Center).into());
                counts.push(caption(&format!("−{}", checkpoint.deletions)).foreground(removed.clone()).opacity(1.0).vertical_alignment(VerticalAlignment::Center).into());
            }
            if checkpoint.files_changed > 0 {
                let source = if checkpoint.diff_source.is_empty() { format!("turn:{}", checkpoint.turn_count) } else { checkpoint.diff_source.clone() };
                let on_act = draw.on_act.clone();
                counts.push(Button::new().content(TextBlock::new().text("View Diff").font_size(theme::CAPTION)).on_click(move || on_act.call(Act::Diff(source.clone()))).tooltip("What this turn changed, in the Diff tab"));
            }
            if checkpoint.can_revert {
                // It changes files in the project: asked about first.
                let (on_act, id, label) = (draw.on_act.clone(), checkpoint.id.clone(), checkpoint.label.clone());
                counts.push(
                    Button::new()
                        .content(TextBlock::new().text("Revert…").font_size(theme::CAPTION))
                        .on_click(move || on_act.call(Act::Revert(id.clone(), label.clone())))
                        .tooltip("Put the files back as they were before this turn"),
                );
            }
            let mut lines: Vec<View> = vec![
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                            theme::glyph("\u{E81C}", 12.0).opacity(0.8).into(),
                            TextBlock::new().text(&checkpoint.label).font_size(theme::CAPTION + 1.0).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into(),
                        ]),
                        StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(8.0).children(counts),
                    ))
                    .into(),
            ];
            for file in &checkpoint.files {
                lines.push(
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
                        .column_spacing(8.0)
                        .children((
                            TextBlock::new().text(&file.path).font_family(theme::MONO).font_size(theme::CAPTION).text_trimming(TextTrimming::CharacterEllipsis),
                            caption(&format!("+{}", file.insertions)).grid_column(1).foreground(added.clone()).opacity(1.0),
                            caption(&format!("−{}", file.deletions)).grid_column(2).foreground(removed.clone()).opacity(1.0),
                        ))
                        .into(),
                );
            }
            card(lines).into()
        }
        "subagent" => {
            let mut buttons: Vec<View> = Vec::new();
            if !item.child_thread_id.is_empty() {
                buttons.push(tell(core, "Open", Event::SelectThread { thread_id: item.child_thread_id.clone() }).into());
            }
            if item.can_stop {
                buttons.push(tell(core, "Stop", Event::StopSubagent { subagent_id: item.id.clone() }).into());
            }
            card(vec![
                Grid::new()
                    .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                    .column_spacing(10.0)
                    .children((
                        theme::glyph("\u{E99A}", 16.0).foreground(theme::accent()),
                        StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
                            TextBlock::new().text(&item.title).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
                            caption(if item.progress.is_empty() { &item.status } else { &item.progress }).into(),
                        ]),
                        StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(6.0).vertical_alignment(VerticalAlignment::Center).children(buttons),
                    ))
                    .into(),
            ])
            .into()
        }
        "error" => {
            let message = [item.text.as_str(), item.title.as_str()].into_iter().find(|s| !s.is_empty()).unwrap_or("The turn failed.");
            let mut lines: Vec<View> = vec![
                StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                    theme::glyph("\u{E783}", 14.0).foreground(theme::critical()).into(),
                    TextBlock::new().text(if item.title.is_empty() || item.text.is_empty() { "Error" } else { item.title.as_str() }).font_weight(FontWeight::SEMI_BOLD).into(),
                ]).into(),
                text(message, size).foreground(theme::critical()).into(),
            ];
            if item.can_retry {
                lines.push(tell(core, "Retry", Event::Retry).horizontal_alignment(HorizontalAlignment::Left).into());
            }
            card(lines).border_brush(theme::critical()).into()
        }
        "artifact" => {
            let title = if item.artifact_title.is_empty() { item.title.as_str() } else { item.artifact_title.as_str() };
            let mut lines: Vec<View> = vec![
                StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                    theme::glyph("\u{E7C3}", 14.0).opacity(0.8).into(),
                    TextBlock::new().text(if title.is_empty() { "Artifact" } else { title }).font_weight(FontWeight::SEMI_BOLD).into(),
                    caption(&format!("version {}", item.artifact_version.max(1))).vertical_alignment(VerticalAlignment::Center).into(),
                ]).into(),
            ];
            if let Some(url) = item.artifact_hosted_url.clone() {
                let on_act = draw.on_act.clone();
                lines.push(Button::new().horizontal_alignment(HorizontalAlignment::Left).content(TextBlock::new().text("Open in the Browser").font_size(theme::CAPTION)).on_click(move || on_act.call(Act::Open(url.clone()))).into());
            }
            card(lines).into()
        }
        "notice" => StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children(vec![theme::glyph("\u{E946}", 12.0).opacity(0.7).into(), caption(if item.text.is_empty() { &item.title } else { &item.text }).into()])
            .into(),
        _ => step(core, item, draw),
    }
}

/// A row with the find's mark on it when it holds a match.
fn marked(item: &ItemView, view: View) -> View {
    if item.find_matches == 0 {
        return view;
    }
    Border::new()
        .padding(Thickness::uniform(4.0))
        .corner_radius(6.0)
        .border_brush(theme::accent())
        .border_thickness(Thickness::uniform(if item.find_active { 2.0 } else { 1.0 }))
        .background(if item.find_active { theme::selected() } else { theme::code() })
        .content(view)
        .into()
}

/// The steps of one turn under their summary; opened, each step.
fn group(core: &CoreLink, steps: &[Rc<ItemView>], draw: &Draw<'_>) -> View {
    let (first, last) = (&steps[0], &steps[steps.len() - 1]);
    // A group holding the find's match opens so the match shows.
    // A group is named after its first step: its own key keeps the two apart.
    let key = format!("group:{}", first.group);
    let open = draw.state.open(&key, false) || steps.iter().any(|s| s.find_active);
    let summary = if first.group_summary.is_empty() { format!("{} steps", steps.len()) } else { first.group_summary.clone() };
    let running = steps.iter().any(|s| s.status == "running");
    let (on_act, id) = (draw.on_act.clone(), key);
    let lead: View = if running { ProgressRing::new().is_active(true).width(12.0).height(12.0).into() } else { theme::glyph("\u{E81E}", 12.0).opacity(0.7).into() };
    let mut head: Vec<View> = vec![
        theme::glyph(if open { "\u{E70D}" } else { "\u{E76C}" }, 9.0).opacity(0.6).into(),
        lead,
        TextBlock::new().text(&summary).font_size(theme::CAPTION + 1.0).opacity(0.8).vertical_alignment(VerticalAlignment::Center).into(),
    ];
    if last.run_duration_ms > 0 {
        head.push(turn_footer(last));
    }
    let mut rows: Vec<View> = vec![
        Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Left)
            .automation_name(&summary)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(7.0).children(head))
            .on_click(move || on_act.call(Act::Toggle(id.clone())))
            .into(),
    ];
    // Folded, the pictures the steps looked at still show (opened, each step shows its own).
    if !open && !first.group_images.is_empty() {
        rows.push(Border::new().padding(Thickness::new(30.0, 0.0, 0.0, 4.0)).content(pictures(&first.group_images, 120.0, draw)).into());
    }
    if open {
        rows.push(
            Border::new()
                .padding(Thickness::new(16.0, 0.0, 0.0, 2.0))
                .content(StackPanel::new().orientation(Orientation::Vertical).children(steps.iter().map(|s| marked(s, row(core, s, draw))).collect::<Vec<View>>()))
                .into(),
        );
    }
    Border::new().background(theme::code()).corner_radius(theme::RADIUS).content(StackPanel::new().orientation(Orientation::Vertical).children(rows)).into()
}

/// An entry of the page: a row, or a turn's steps together.
pub fn entry(core: &CoreLink, entry: &Entry<'_>, draw: &Draw<'_>) -> View {
    match entry {
        Entry::Group(steps) => group(core, steps, draw),
        Entry::Row(item) => {
            let view = marked(item, row(core, item, draw));
            // A reply says when its turn ran in its own footer; another last row gets one here.
            if item.run_duration_ms > 0 && item.kind != "assistant" {
                StackPanel::new().orientation(Orientation::Vertical).spacing(2.0).children(vec![view, turn_footer(item)]).into()
            } else {
                view
            }
        }
    }
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
