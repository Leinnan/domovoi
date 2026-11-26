//! Collect complete, passive history and retain a pending live escape sequence.

use super::History;

const MAX_SEQUENCE: usize = 4096;

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    String {
        osc: bool,
        escape: bool,
    },
}

#[derive(Default)]
pub(super) struct Replay {
    state: State,
    prefix: String,
}

impl Replay {
    /// The fragment must come after a synthetic redraw so the next live bytes
    /// finish the sequence in the same parser state as the host.
    pub(super) fn pending(&self) -> &str {
        &self.prefix
    }

    pub(super) fn push(&mut self, history: &mut History, text: &str) {
        let mut plain_start = 0;
        for (index, ch) in text.char_indices() {
            if matches!(self.state, State::Ground) {
                if ch != '\x1b' {
                    continue;
                }
                history.push(&text[plain_start..index]);
                self.prefix.push(ch);
                self.state = State::Escape;
                plain_start = index + 1;
                continue;
            }

            plain_start = index + ch.len_utf8();
            if matches!(ch, '\x18' | '\x1a') {
                self.finish(history, false);
                continue;
            }
            if self.prefix.len() < MAX_SEQUENCE {
                self.prefix.push(ch);
            }

            match self.state {
                State::Ground => unreachable!(),
                State::Escape => match ch {
                    '[' => self.state = State::Csi,
                    ']' | 'P' | 'X' | '^' | '_' => {
                        self.state = State::String {
                            osc: ch == ']',
                            escape: false,
                        }
                    }
                    ' '..='/' => {}
                    '\x1b' => {
                        self.prefix.clear();
                        self.prefix.push(ch);
                    }
                    _ => self.finish(history, !matches!(ch, 'Z')),
                },
                State::Csi => match ch {
                    '@'..='~' => self.finish(history, !matches!(ch, 'c' | 'n' | 'p' | 't')),
                    '\x1b' => {
                        self.prefix.clear();
                        self.prefix.push(ch);
                        self.state = State::Escape;
                    }
                    _ => {}
                },
                State::String { osc, escape } => {
                    if (escape && ch == '\\') || (osc && ch == '\x07') {
                        self.finish(history, false);
                    } else if escape && ch != '\x1b' {
                        self.prefix.clear();
                        self.prefix.push('\x1b');
                        self.prefix.push(ch);
                        self.state = match ch {
                            '[' => State::Csi,
                            ']' | 'P' | 'X' | '^' | '_' => State::String {
                                osc: ch == ']',
                                escape: false,
                            },
                            _ => State::Escape,
                        };
                        if matches!(self.state, State::Escape) && ch > '/' {
                            self.finish(history, false);
                        }
                    } else {
                        self.state = State::String {
                            osc,
                            escape: ch == '\x1b',
                        };
                    }
                }
            }
        }
        if matches!(self.state, State::Ground) {
            history.push(&text[plain_start..]);
        }
    }

    fn finish(&mut self, history: &mut History, keep: bool) {
        if keep && self.prefix.len() < MAX_SEQUENCE && !self.prefix.contains(['\n', '\r']) {
            history.push(&self.prefix);
        }
        self.prefix.clear();
        self.state = State::Ground;
    }
}

pub(super) fn append_redraw(history: &mut String, screen: &vt100::Screen) {
    // Both emulators must start on the normal buffer before it is repainted.
    history.push_str("\x18\x1b[?1049l\x1b[?1;9;1000;1002;1003;1005;1006;2004l\x1b>");
    if screen.alternate_screen() {
        let (rows, cols) = screen.size();
        let mut normal = vt100::Parser::new(rows, cols, 0);
        *normal.screen_mut() = screen.clone();
        normal.process(b"\x1b[?1049l");
        history.push_str(&normal.screen().state_formatted());
        history.push_str("\x1b[?1049h");
    }
    history.push_str(&screen.state_formatted());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_restores_both_buffers_and_live_input_modes() {
        let mut host = vt100::Parser::default();
        host.process(b"shell\x1b[?1049hfull screen\x1b[?2004h\x1b[?1h");
        let mut snapshot = String::new();
        append_redraw(&mut snapshot, host.screen());
        let mut viewer = vt100::Parser::default();
        viewer.process(snapshot.as_bytes());
        assert!(viewer.screen().alternate_screen());
        assert!(viewer.screen().bracketed_paste());
        assert!(viewer.screen().application_cursor());
        assert_eq!(viewer.screen().contents(), "full screen");
        viewer.process(b"\x1b[?1049l");
        assert_eq!(viewer.screen().contents(), "shell");
    }

    #[test]
    fn replay_holds_partials_and_drops_queries_and_clipboard_commands() {
        let mut history = History::default();
        let mut replay = Replay::default();
        replay.push(&mut history, "text\x1b[");
        assert_eq!(history.tail(), "text");
        assert_eq!(replay.pending(), "\x1b[");
        replay.push(&mut history, "6n\x1b]52;c;data\x1b");
        replay.push(&mut history, "\\more\x1b[1m!");
        assert!(replay.pending().is_empty());
        assert_eq!(history.tail(), "textmore\x1b[1m!");
    }
}
