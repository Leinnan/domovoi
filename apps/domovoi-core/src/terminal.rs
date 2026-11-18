//! Terminal emulation in the core: the server streams raw PTY output; a
//! `vt100` parser per terminal keeps the screen, which the view model hands to
//! shells as styled runs. Shells only draw cells and send keystrokes.

use crate::view::{TerminalLineView, TerminalScreenView, TerminalSpanView};

use std::num::NonZeroU16;
use domovoi_model::typed::TerminalAttachEvent;

pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 100;

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

pub struct Session {
    pub id: String,
    pub label: String,
    parser: vt100::Parser,
    /// The screen as last drawn for the view; cleared by anything that changes it.
    drawn: std::sync::Mutex<Option<TerminalScreenView>>,
    pub status: String,
    pub last_sequence: u64,
    /// How many rows of history there are above the screen.
    history: usize,
}

impl Session {
    pub fn new(id: &str, label: &str) -> Self {
        Session {
            id: id.into(),
            label: label.into(),
            parser: vt100::Parser::new(nz(DEFAULT_ROWS), nz(DEFAULT_COLS), 2_000),
            drawn: std::sync::Mutex::default(),
            status: "starting".into(),
            last_sequence: 0,
            history: 0,
        }
    }

    /// The emulated screen (cursor modes, contents).
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    fn changed(&mut self) {
        *self.drawn.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Applies one `terminal.attach` stream event.
    pub fn apply(&mut self, event: &TerminalAttachEvent) {
        self.changed();
        match event {
            TerminalAttachEvent::Snapshot { snapshot } => {
                // The server's size, held to what a resize allows: the
                // emulator allocates every cell up front.
                let rows = snapshot.rows.unwrap_or(DEFAULT_ROWS as u64).clamp(2, 500) as u16;
                let cols = snapshot.cols.unwrap_or(DEFAULT_COLS as u64).clamp(10, 1000) as u16;
                self.parser = vt100::Parser::new(nz(rows), nz(cols), 2_000);
                self.parser.process(snapshot.history.as_bytes());
                self.status = snapshot.status.as_str().to_owned();
                self.last_sequence = snapshot.sequence.unwrap_or(0);
            }
            TerminalAttachEvent::Output { sequence, data, .. } => {
                let seq = sequence.unwrap_or(0);
                if seq == 0 || seq > self.last_sequence {
                    self.parser.process(data.as_bytes());
                    self.last_sequence = seq;
                    if self.status == "starting" {
                        self.status = "running".into();
                    }
                }
            }
            TerminalAttachEvent::Exited { .. } => self.status = "exited".into(),
            TerminalAttachEvent::Closed { .. } => self.status = "closed".into(),
            TerminalAttachEvent::Cleared { .. } => self.parser.process(b"\x1b[H\x1b[2J"),
            TerminalAttachEvent::Error { .. } => self.status = "error".into(),
            TerminalAttachEvent::Restarted { .. } | TerminalAttachEvent::Activity { .. } | TerminalAttachEvent::Other(_) => {}
        }
        // Output while looking back doesn't pull the view down (a command
        // still printing would make reading impossible); a key or a paste
        // returns to the live screen.
        self.measure_history();
    }

    /// Counts the history: vt100 clamps an offset to it, so ask for all of
    /// it and read back how far it went.
    fn measure_history(&mut self) {
        let at = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        self.history = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(at);
    }

    /// Moves the view `lines` rows back into the history (negative: towards
    /// the live screen), within what there is.
    pub fn scroll(&mut self, lines: i32) {
        self.changed();
        let at = self.parser.screen().scrollback() as i64;
        let to = (at + lines as i64).clamp(0, self.history as i64) as usize;
        self.parser.screen_mut().set_scrollback(to);
    }

    /// Back to the live screen (a key, a paste).
    pub fn scroll_to_bottom(&mut self) {
        self.changed();
        self.parser.screen_mut().set_scrollback(0);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.changed();
        self.parser.screen_mut().set_size(nz(rows.max(2)), nz(cols.max(10)));
        self.measure_history();
    }

    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().size();
        (r.get(), c.get())
    }

    pub fn running(&self) -> bool {
        matches!(self.status.as_str(), "starting" | "running")
    }

    /// The screen as styled runs, drawn again only after it changed.
    pub fn screen_view(&self) -> TerminalScreenView {
        let mut drawn = self.drawn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drawn.get_or_insert_with(|| self.draw()).clone()
    }

    fn draw(&self) -> TerminalScreenView {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get(), cols.get());
        let mut lines = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut spans: Vec<TerminalSpanView> = vec![];
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.has_contents() { cell.contents().to_owned() } else { " ".into() };
                let style = TerminalSpanView {
                    text: String::new(),
                    fg: color(cell.fgcolor()),
                    bg: color(cell.bgcolor()),
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                };
                match spans.last_mut() {
                    Some(last) if same_style(last, &style) => last.text.push_str(&text),
                    _ => spans.push(TerminalSpanView { text, ..style }),
                }
            }
            // Trailing default-styled blanks carry nothing.
            if let Some(last) = spans.last_mut() {
                if last.bg == -1 && !last.inverse {
                    let trimmed = last.text.trim_end().len();
                    last.text.truncate(trimmed);
                }
            }
            spans.retain(|s| !s.text.is_empty());
            lines.push(TerminalLineView { spans });
        }
        let (cr, cc) = screen.cursor_position();
        let back = screen.scrollback();
        TerminalScreenView {
            id: self.id.clone(),
            rows: rows as u32,
            cols: cols as u32,
            lines,
            cursor_row: cr as u32,
            cursor_col: cc as u32,
            // Looking back: the cursor's row isn't on screen.
            cursor_visible: !screen.hide_cursor() && self.running() && back == 0,
            exited: !self.running(),
            status: self.status.clone(),
            scrollback_offset: back as u32,
            scrollback_max: self.history as u32,
        }
    }
}

fn same_style(a: &TerminalSpanView, b: &TerminalSpanView) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.bold == b.bold && a.italic == b.italic && a.underline == b.underline && a.inverse == b.inverse
}

fn color(c: vt100::Color) -> i32 {
    match c {
        vt100::Color::Default => -1,
        vt100::Color::Idx(i) => i as i32,
        vt100::Color::Rgb(r, g, b) => 0x100_0000 | ((r as i32) << 16) | ((g as i32) << 8) | b as i32,
    }
}

/// The bytes a key sends, from the shell's key name and modifiers.
pub fn key_bytes(key: &str, text: &str, ctrl: bool, alt: bool, app_cursor: bool) -> Vec<u8> {
    let arrow = |c: u8| if app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let mut out = match key {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "backtab" => b"\x1b[Z".to_vec(),
        "backspace" => vec![0x7f],
        "escape" => vec![0x1b],
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        _ if ctrl && text.len() == 1 => {
            let c = text.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() { vec![c - b'a' + 1] } else { text.as_bytes().to_vec() }
        }
        _ => text.as_bytes().to_vec(),
    };
    if alt && !out.is_empty() && out[0] != 0x1b {
        out.insert(0, 0x1b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `terminal.attach` event of terminal `t` of thread `t1`, from the
    /// fields a test is about.
    fn event(mut given: serde_json::Value) -> TerminalAttachEvent {
        let fields = given.as_object_mut().expect("an event");
        fields.entry("threadId").or_insert(json!("t1"));
        fields.entry("terminalId").or_insert(json!("t"));
        if let Some(snapshot) = fields.get_mut("snapshot").and_then(serde_json::Value::as_object_mut) {
            for (key, value) in [("threadId", json!("t1")), ("terminalId", json!("t")), ("cwd", json!("/w")), ("worktreePath", json!(null)), ("pid", json!(1)), ("exitCode", json!(null)), ("exitSignal", json!(null)), ("label", json!("Terminal")), ("updatedAt", json!("1"))] {
                snapshot.entry(key).or_insert(value);
            }
        }
        TerminalAttachEvent::from_value(given).expect("the event fits the schema")
    }

    #[test]
    fn renders_colored_output_and_tracks_status() {
        let mut s = Session::new("t1", "Terminal 1");
        s.apply(&event(json!({ "type": "snapshot", "snapshot": { "rows": 4, "cols": 20, "history": "$ ls\r\n", "status": "running", "sequence": 3 } })));
        s.apply(&event(json!({ "type": "output", "sequence": 2, "data": "stale" })));
        s.apply(&event(json!({ "type": "output", "sequence": 4, "data": "\x1b[31mred\x1b[0m ok" })));
        let v = s.screen_view();
        assert_eq!(v.lines[0].spans[0].text, "$ ls");
        assert_eq!(v.lines[1].spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["red", " ok"]);
        assert_eq!(v.lines[1].spans[0].fg, 1);
        assert!(!v.lines.iter().any(|l| l.spans.iter().any(|s| s.text.contains("stale"))), "older sequences are ignored");
        s.apply(&event(json!({ "type": "exited", "exitCode": 0, "exitSignal": null })));
        assert!(s.screen_view().exited);
    }

    #[test]
    fn a_huge_snapshot_size_is_held_to_what_a_resize_allows() {
        let mut s = Session::new("t1", "Terminal 1");
        s.apply(&event(json!({ "type": "snapshot", "snapshot": { "rows": 70_000, "cols": 70_000, "history": "", "status": "running", "sequence": 1 } })));
        assert_eq!(s.size(), (500, 1000));
        s.apply(&event(json!({ "type": "snapshot", "snapshot": { "rows": 0, "cols": 0, "history": "", "status": "running", "sequence": 2 } })));
        assert_eq!(s.size(), (2, 10));
    }

    #[test]
    fn scrolls_back_through_history_and_returns_on_output() {
        let mut s = Session::new("t1", "Terminal 1");
        s.apply(&event(json!({ "type": "snapshot", "snapshot": { "rows": 10, "cols": 20, "history": "", "status": "running", "sequence": 1 } })));
        let lines: String = (1..=200).map(|i| format!("line {i}\r\n")).collect();
        s.apply(&event(json!({ "type": "output", "sequence": 2, "data": lines })));
        let live = s.screen_view();
        assert_eq!(live.scrollback_offset, 0);
        assert!(live.scrollback_max >= 190, "history above the screen: {}", live.scrollback_max);
        assert!(live.cursor_visible);

        s.scroll(i32::MAX);
        let top = s.screen_view();
        assert_eq!(top.scrollback_offset, top.scrollback_max, "clamped to the history");
        assert_eq!(top.lines[0].spans[0].text, "line 1");
        assert!(!top.cursor_visible, "no cursor while looking back");

        s.scroll(-5);
        assert_eq!(s.screen_view().scrollback_offset, top.scrollback_max - 5);
        s.scroll(-1_000);
        assert_eq!(s.screen_view().scrollback_offset, 0, "never below the live screen");

        s.scroll(20);
        s.apply(&event(json!({ "type": "output", "sequence": 3, "data": "more\r\n" })));
        assert!(s.screen_view().scrollback_offset > 0, "output doesn't pull the view down");
        s.scroll_to_bottom();
        assert_eq!(s.screen_view().scrollback_offset, 0);
    }

    #[test]
    fn keys_map_to_terminal_bytes() {
        assert_eq!(key_bytes("char", "c", true, false, false), vec![3]);
        assert_eq!(key_bytes("up", "", false, false, true), b"\x1bOA");
        assert_eq!(key_bytes("char", "x", false, true, false), b"\x1bx");
        assert_eq!(key_bytes("enter", "", false, false, false), b"\r");
    }
}
