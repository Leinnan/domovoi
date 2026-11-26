//! The window's keyboard shortcuts, as the Iced app has them (Ctrl for its
//! Mod).

use windows_reactor::{InputModifiers, VirtualKey};

/// What a shortcut does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewThread,
    ScratchThread,
    Settings,
    ToggleSidebar,
    /// Ends the turn under way.
    Stop,
    /// Opens the find box of the open thread, and goes to its next or previous match.
    Find,
    FindNext,
    FindPrevious,
    /// Shuts what is open on top: the palette, the picture viewer, the find box, Settings.
    Back,
    /// Opens the command palette, or shuts it: on its commands, on the workspace's files, on what the files hold.
    Palette,
    PaletteFiles,
    PaletteContent,
    /// Shows the panel beside the thread, or hides it; and shows one of its tabs.
    TogglePanel,
    Panel(&'static str),
}

const COMMA: VirtualKey = VirtualKey(0xBC);
const PERIOD: VirtualKey = VirtualKey(0xBE);
const B: VirtualKey = VirtualKey(0x42);
const F: VirtualKey = VirtualKey(0x46);
const G: VirtualKey = VirtualKey(0x47);
const N: VirtualKey = VirtualKey(0x4E);
const A: VirtualKey = VirtualKey(0x41);
const D: VirtualKey = VirtualKey(0x44);
const E: VirtualKey = VirtualKey(0x45);
const J: VirtualKey = VirtualKey(0x4A);
const K: VirtualKey = VirtualKey(0x4B);
const P: VirtualKey = VirtualKey(0x50);
const T: VirtualKey = VirtualKey(0x54);

/// The command `key` with `modifiers` is, if any.
pub fn command(key: VirtualKey, modifiers: InputModifiers) -> Option<Command> {
    let held = |modifier| modifiers.contains(modifier);
    let (ctrl, shift, alt) = (held(InputModifiers::CONTROL), held(InputModifiers::SHIFT), held(InputModifiers::ALT));
    if key == VirtualKey::ESCAPE && !ctrl && !shift && !alt {
        return Some(Command::Back);
    }
    if !ctrl {
        return None;
    }
    if alt {
        // With Alt too, only the repository's tab; AltGr types a letter with most others.
        return (key == G && !shift).then_some(Command::Panel("git"));
    }
    match (key, shift) {
        (N, false) => Some(Command::NewThread),
        (N, true) => Some(Command::ScratchThread),
        (COMMA, false) => Some(Command::Settings),
        (B, false) => Some(Command::ToggleSidebar),
        (PERIOD, false) => Some(Command::Stop),
        (F, false) => Some(Command::Find),
        (G, false) => Some(Command::FindNext),
        (G, true) => Some(Command::FindPrevious),
        (K, false) => Some(Command::Palette),
        (P, false) => Some(Command::PaletteFiles),
        (F, true) => Some(Command::PaletteContent),
        (J, false) => Some(Command::TogglePanel),
        (D, true) => Some(Command::Panel("diff")),
        (A, true) => Some(Command::Panel("artifacts")),
        (T, true) => Some(Command::Panel("terminal")),
        (E, true) => Some(Command::Panel("files")),
        _ => None,
    }
}

impl Command {
    /// Whether the key is used up. Escape is not: whatever has the keyboard
    /// (a text box, a list) may want it too.
    pub fn handled(self) -> bool {
        self != Command::Back
    }

    /// Whether the shortcut still works while the terminal has the keyboard.
    /// A shell has its own use for Ctrl with a letter and for Escape, so only
    /// the chords with Shift or Alt do.
    pub fn over_terminal(self) -> bool {
        matches!(self, Command::ScratchThread | Command::FindPrevious | Command::PaletteContent | Command::Panel(_))
    }
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
    fn a_shortcut_needs_ctrl_and_shift_tells_two_apart() {
        let ctrl = with(&[InputModifiers::CONTROL]);
        assert_eq!(command(N, ctrl), Some(Command::NewThread));
        assert_eq!(command(N, with(&[InputModifiers::CONTROL, InputModifiers::SHIFT])), Some(Command::ScratchThread));
        assert_eq!(command(COMMA, ctrl), Some(Command::Settings));
        assert_eq!(command(PERIOD, ctrl), Some(Command::Stop));
        assert_eq!(command(F, ctrl), Some(Command::Find));
        assert_eq!(command(G, with(&[InputModifiers::CONTROL, InputModifiers::SHIFT])), Some(Command::FindPrevious));
        assert_eq!(command(N, InputModifiers::NONE), None, "typing an n is typing");
        assert_eq!(command(N, with(&[InputModifiers::CONTROL, InputModifiers::ALT])), None, "AltGr types a letter on some keyboards");
        assert_eq!(command(K, ctrl), Some(Command::Palette));
        assert_eq!(command(F, with(&[InputModifiers::CONTROL, InputModifiers::SHIFT])), Some(Command::PaletteContent));
        assert_eq!(command(T, with(&[InputModifiers::CONTROL, InputModifiers::SHIFT])), Some(Command::Panel("terminal")));
        assert_eq!(command(G, with(&[InputModifiers::CONTROL, InputModifiers::ALT])), Some(Command::Panel("git")));
        assert!(Command::Panel("diff").over_terminal() && !Command::NewThread.over_terminal() && !Command::Back.over_terminal());
    }

    #[test]
    fn escape_goes_back_and_stays_for_others_to_use() {
        assert_eq!(command(VirtualKey::ESCAPE, InputModifiers::NONE), Some(Command::Back));
        assert!(!Command::Back.handled());
        assert!(Command::Settings.handled());
    }
}
