//! Interactive Terminal pane powered by `domovoi_core`'s `vt100` emulator
//! and rendered via a custom Iced Canvas with exact cell alignment, 256-color
//! and truecolor support, keyboard input, scrollback, and automatic resize.

use domovoi_core::Event;
use domovoi_core::view::TerminalScreenView;
use iced::keyboard::{self, Key, key};
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Geometry, Path, Text as CanvasText};
use iced::widget::{Space, button, column, container, pane_grid, row, stack, text};
use iced::{Alignment, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{alpha, colors, xterm_color};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

const CELL_W: f32 = 7.4;
const CELL_H: f32 = 16.0;
const PAD: f32 = 8.0;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let term = &app.vm.panel.terminal;

    let mut tab_row = row![].spacing(XS).align_y(Alignment::Center);
    for t in &term.tabs {
        let running = t.running;
        let active = t.active;
        tab_row = tab_row.push(
            container(
                row![
                    w::dot(move |c| if running { c.success } else { c.faint }, 6.0),
                    button(text(&t.label).size(TEXT_SMALL).font(if active { fonts::SEMIBOLD } else { fonts::INTER }))
                        .padding([2.0, XS])
                        .style(w::ghost_style)
                        .on_press(core(Event::SelectTerminal { terminal_id: t.id.clone() })),
                    button(w::themed_icon(Icon::X, ICON_XS, |c| c.muted))
                        .padding(2.0)
                        .style(w::ghost_style)
                        .on_press(core(Event::CloseTerminal { terminal_id: t.id.clone() })),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            )
            .padding([2.0, TIGHT])
            .style(if active { w::card_style } else { w::surface }),
        );
    }

    if let Some(screen) = term.active.as_ref().filter(|screen| !screen.exited) {
        let label = if screen.watching { "Interact" } else { "Watch" };
        tab_row = tab_row.push(w::small_button(label, w::secondary_style, Some(core(Event::SetTerminalWatch { watching: !screen.watching }))));
    }
    if term.active.as_ref().is_some_and(|screen| screen.exited) {
        tab_row = tab_row.push(w::small_button("Open fresh shell", w::secondary_style, Some(core(Event::ReopenTerminal))));
    }
    if term.active.is_some() {
        for (label, mode) in [("Side by side", "sideBySide"), ("Stacked", "stacked"), ("Single pane", "single")] {
            let already = match (&term.split, mode) {
                (None, "single") => true,
                (Some(split), "sideBySide") => !split.stacked,
                (Some(split), "stacked") => split.stacked,
                _ => false,
            };
            if !already {
                tab_row = tab_row.push(w::small_button(label, w::ghost_style, Some(core(Event::SetTerminalSplit { mode: mode.into() }))));
            }
        }
    }
    if term.can_rename && let Some(tab) = term.tabs.iter().find(|tab| tab.active) {
        if let Some((_, draft)) = app.terminal_rename.as_ref().filter(|(id, _)| *id == tab.id) {
            tab_row = tab_row
                .push(w::input("Terminal name", draft).on_input(Message::TerminalNameChanged).on_submit(Message::SaveTerminalName).width(140))
                .push(w::small_button("Cancel", w::ghost_style, Some(Message::CancelTerminalName)));
        } else {
            tab_row = tab_row.push(w::small_button("Rename", w::ghost_style,
                Some(Message::EditTerminalName(tab.id.clone(), tab.label.clone()))));
        }
    }
    let writable = term.active.as_ref().filter(|screen| !screen.watching && !screen.exited);
    tab_row = tab_row
        .push(Space::new().width(Length::Fill))
        .push(w::icon_button(Icon::Copy, "Copy screen", term.active.as_ref().map(|_| Message::TerminalCopyScreen)))
        .push(w::icon_button(Icon::ClipboardList, "Paste", writable.map(|_| Message::TerminalPasteClipboard)))
        .push(w::icon_button(Icon::Eraser, "Clear terminal", writable.map(|_| core(Event::ClearTerminal))))
        .push(w::icon_button(Icon::Plus, "New terminal", Some(core(Event::NewTerminal))));

    let body: Element<'_, Message> = if let Some(split) = &term.split {
        let active = term.active.as_ref().map(|screen| screen.id.as_str());
        pane_grid::PaneGrid::new(&app.terminal_grid.panes, |_, second, _| {
            let screen = if *second { &split.right } else { &split.left };
            let focused = app.terminal_focused && active == Some(screen.id.as_str());
            pane_grid::Content::new(terminal_canvas(screen, focused, true))
        })
        .spacing(6)
        .on_resize(5, |event| Message::ResizeTerminalDivider(event.ratio))
        .into()
    } else if let Some(screen) = &term.active {
        let canvas_el = terminal_canvas(screen, app.terminal_focused, false);

        let mut layers = vec![canvas_el];
        if screen.scrollback_offset > 0 {
            let offset = screen.scrollback_offset;
            layers.push(
                container(
                    w::small_button(
                        format!("{offset} of {} lines back · Jump to live", screen.scrollback_max.max(offset)),
                        w::secondary_style,
                        Some(Message::TerminalScroll(-(offset as i32))),
                    ),
                )
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(S)
                .into(),
            );
        }
        if screen.exited {
            layers.push(
                container(w::badge_capsule(
                    if screen.has_saved_screen { "Last saved screen" } else if screen.restored { "Ended after server restart" } else { "Process exited" },
                    Some(|c| c.muted),
                ))
                    .align_right(Length::Fill)
                    .align_bottom(Length::Fill)
                    .padding(S)
                    .into(),
            );
        }
        stack(layers).width(Length::Fill).height(Length::Fill).into()
    } else {
        w::empty_state(
            Icon::Terminal,
            "No terminal",
            if app.vm.thread.is_none() {
                "Open a thread to use terminals."
            } else {
                "Click + to open a terminal in the thread's workspace."
            },
            app.vm.thread.as_ref().map(|_| w::icon_label_button(Icon::Plus, "New Terminal", w::Kind::Primary, Some(core(Event::NewTerminal))).into()),
        )
    };

    column![container(tab_row).padding([S, CHROME_PAD_X]), w::divider(), body].height(Length::Fill).into()
}

fn terminal_canvas(screen: &TerminalScreenView, focused: bool, split: bool) -> Element<'_, Message> {
    Canvas::new(TerminalCanvas {
        screen: screen.clone(),
        focused,
        split,
    }).width(Length::Fill).height(Length::Fill).into()
}

pub fn plain_text(screen: &TerminalScreenView) -> String {
    let mut rows: Vec<String> = screen
        .lines
        .iter()
        .map(|l| {
            let s: String = l.spans.iter().map(|sp| sp.text.as_str()).collect();
            s.trim_end().to_owned()
        })
        .collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows.join("\n")
}

struct TerminalCanvas {
    screen: TerminalScreenView,
    focused: bool,
    split: bool,
}

#[derive(Default)]
struct TerminalCanvasState {
    terminal_id: String,
    last_size: Option<(u32, u32)>,
}

impl canvas::Program<Message> for TerminalCanvas {
    type State = TerminalCanvasState;

    fn update(&self, state: &mut Self::State, event: &iced::Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        // Resize check when bounds change.
        let cols = (((bounds.width - PAD * 2.0) / CELL_W).floor() as i32).clamp(10, 1000) as u32;
        let rows = (((bounds.height - PAD * 2.0) / CELL_H).floor() as i32).clamp(2, 500) as u32;
        if state.terminal_id != self.screen.id {
            state.terminal_id.clone_from(&self.screen.id);
            state.last_size = None;
        }
        if self.screen.watching || self.screen.exited {
            state.last_size = None;
        } else if state.last_size != Some((rows, cols)) {
            state.last_size = Some((rows, cols));
            return Some(canvas::Action::publish(Message::TerminalResize { terminal_id: self.screen.id.clone(), rows, cols }));
        }

        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let inside = cursor.is_over(bounds);
                if self.split && inside && !self.focused {
                    return Some(canvas::Action::publish(Message::FocusTerminalPane(self.screen.id.clone())).and_capture());
                }
                if !self.split && inside != self.focused {
                    return Some(canvas::Action::publish(Message::TerminalFocus(inside)));
                }
            }
            iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) && (!self.split || self.focused) => {
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => (*y * 3.0).round() as i32,
                    mouse::ScrollDelta::Pixels { y, .. } => (*y / CELL_H).round() as i32,
                };
                if lines != 0 {
                    return Some(canvas::Action::publish(Message::TerminalScroll(lines)).and_capture());
                }
            }
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, text, .. }) if self.focused => {
                if modifiers.command() {
                    if let Key::Character(c) = key {
                        match c.to_ascii_lowercase().as_str() {
                            "v" => return Some(canvas::Action::publish(Message::TerminalPasteClipboard).and_capture()),
                            "c" => return Some(canvas::Action::publish(Message::TerminalCopyScreen).and_capture()),
                            _ => return None,
                        }
                    }
                    return None;
                }
                if let Some((name, txt)) = map_terminal_key(key, *modifiers, text.as_deref()) {
                    return Some(
                        canvas::Action::publish(Message::TerminalKey {
                            key: name,
                            text: txt,
                            ctrl: modifiers.control(),
                            alt: modifiers.alt(),
                        })
                        .and_capture(),
                    );
                }
            }
            _ => {}
        }
        None
    }

    fn draw(&self, _state: &Self::State, renderer: &Renderer, theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry<Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let h = colors(theme);
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), h.sunken);

        let default_fg = h.text;
        let default_bg = h.sunken;

        for (r_idx, line) in self.screen.lines.iter().enumerate() {
            let y = PAD + (r_idx as f32) * CELL_H;
            if y > bounds.height {
                break;
            }
            let mut col_offset = 0usize;
            for sp in &line.spans {
                let chars = sp.text.chars().count();
                let x = PAD + (col_offset as f32) * CELL_W;
                let w_px = (chars as f32) * CELL_W;
                let mut fg = xterm_color(sp.fg);
                let mut bg = xterm_color(sp.bg);
                if sp.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                    if fg.is_none() {
                        fg = Some(default_bg);
                    }
                    if bg.is_none() {
                        bg = Some(default_fg);
                    }
                }
                if let Some(bg_col) = bg {
                    frame.fill_rectangle(Point::new(x, y), Size::new(w_px, CELL_H), bg_col);
                }
                let font = match (sp.bold, sp.italic) {
                    (true, _) => fonts::MONO_BOLD,
                    (false, true) => fonts::MONO_ITALIC,
                    (false, false) => fonts::MONO,
                };
                frame.fill_text(CanvasText {
                    content: sp.text.clone(),
                    position: Point::new(x, y + 1.0),
                    color: fg.unwrap_or(default_fg),
                    size: iced::Pixels(12.0),
                    font,
                    ..CanvasText::default()
                });
                if sp.underline {
                    let ul = Path::line(Point::new(x, y + CELL_H - 1.5), Point::new(x + w_px, y + CELL_H - 1.5));
                    frame.stroke(&ul, canvas::Stroke::default().with_width(1.0).with_color(fg.unwrap_or(default_fg)));
                }
                col_offset += chars;
            }
        }

        if self.screen.cursor_visible {
            let cx = PAD + (self.screen.cursor_col as f32) * CELL_W;
            let cy = PAD + (self.screen.cursor_row as f32) * CELL_H;
            let cursor_col = alpha(h.accent, if self.focused { 0.72 } else { 0.30 });
            frame.fill_rectangle(Point::new(cx, cy), Size::new(CELL_W, CELL_H), cursor_col);
        }

        vec![frame.into_geometry()]
    }
}

pub fn map_terminal_key(key: &Key, modifiers: keyboard::Modifiers, text: Option<&str>) -> Option<(String, String)> {
    match key {
        Key::Named(named) => {
            let name = match named {
                key::Named::Enter => "enter",
                key::Named::Tab => {
                    if modifiers.shift() {
                        "backtab"
                    } else {
                        "tab"
                    }
                }
                key::Named::Backspace => "backspace",
                key::Named::Escape => "escape",
                key::Named::ArrowUp => "up",
                key::Named::ArrowDown => "down",
                key::Named::ArrowRight => "right",
                key::Named::ArrowLeft => "left",
                key::Named::Home => "home",
                key::Named::End => "end",
                key::Named::PageUp => "pageup",
                key::Named::PageDown => "pagedown",
                key::Named::Delete => "delete",
                key::Named::Space => return Some(("char".into(), " ".into())),
                _ => return None,
            };
            Some((name.into(), String::new()))
        }
        Key::Character(c) => {
            let payload = if modifiers.control() {
                c.to_string()
            } else {
                text.unwrap_or(c.as_str()).to_owned()
            };
            Some(("char".into(), payload))
        }
        Key::Unidentified => None,
    }
}
