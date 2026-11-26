//! Colours the desktop clients share, as plain `0xRRGGBB` so no toolkit's
//! colour type is needed here.

/// The names a project icon's colour goes by (`ProjectIconView::color`), as
/// the web app and the other clients name them.
pub const PROJECT_KEYS: [&str; 18] = [
    "gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan", "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink",
    "rose",
];

const PROJECT_LIGHT: [u32; 18] = [
    0x4b5563, 0x991b1b, 0x9a3412, 0x92400e, 0x854d0e, 0x3f6212, 0x166534, 0x065f46, 0x115e59, 0x155e75, 0x075985, 0x1e40af, 0x4338ca, 0x6d28d9, 0x7e22ce,
    0xa21caf, 0x9d174d, 0x9f1239,
];

const PROJECT_DARK: [u32; 18] = [
    0x9ca3af, 0xfca5a5, 0xfb923c, 0xfbbf24, 0xfacc15, 0xa3e635, 0x4ade80, 0x34d399, 0x2dd4bf, 0x22d3ee, 0x38bdf8, 0x60a5fa, 0xa5b4fc, 0xc4b5fd, 0xd8b4fe,
    0xe879f9, 0xf9a8d4, 0xfda4af,
];

/// A project colour for a light or dark window; an unknown name is gray.
pub fn project_rgb(key: &str, dark: bool) -> u32 {
    let index = PROJECT_KEYS.iter().position(|k| *k == key).unwrap_or(0);
    if dark { PROJECT_DARK[index] } else { PROJECT_LIGHT[index] }
}

/// The 16 colours a terminal names.
const ANSI: [u32; 16] = [
    0x000000, 0xcc3030, 0x0dbd78, 0xe6e60f, 0x2473c7, 0xbd40bd, 0x12a8cc, 0xe6e6e6, 0x666666, 0xf24d4d, 0x24d18c, 0xf5f542, 0x3b8feb, 0xd670d6, 0x29b8db,
    0xffffff,
];

/// A terminal colour as the server sends it: negative is the terminal's own
/// colour (`None`), 0 to 255 an xterm palette entry, and `0x1000000 |
/// 0xRRGGBB` a colour given outright.
pub fn xterm_rgb(value: i32) -> Option<u32> {
    let index = u32::try_from(value).ok()?;
    Some(match index {
        0x100_0000.. => index & 0xFF_FFFF,
        0..16 => ANSI[index as usize],
        16..232 => {
            // A 6 by 6 by 6 cube: each step is 40 apart above the first.
            let level = |step: u32| if step == 0 { 0 } else { step * 40 + 55 };
            let cube = index - 16;
            level(cube / 36) << 16 | level(cube / 6 % 6) << 8 | level(cube % 6)
        }
        232..256 => {
            let gray = (index - 232) * 10 + 8;
            gray << 16 | gray << 8 | gray
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_colour_is_its_own_a_palette_entry_or_given_outright() {
        assert_eq!(xterm_rgb(-1), None);
        assert_eq!(xterm_rgb(1), Some(0xcc3030));
        assert_eq!(xterm_rgb(16), Some(0x000000));
        assert_eq!(xterm_rgb(231), Some(0xffffff));
        assert_eq!(xterm_rgb(196), Some(0xff0000));
        assert_eq!(xterm_rgb(232), Some(0x080808));
        assert_eq!(xterm_rgb(255), Some(0xeeeeee));
        assert_eq!(xterm_rgb(0x100_0000 | 0x12_34_56), Some(0x123456));
        assert_eq!(xterm_rgb(300), None);
    }

    #[test]
    fn a_colour_differs_by_theme_and_an_unknown_one_is_gray() {
        assert_eq!(project_rgb("green", false), 0x166534);
        assert_eq!(project_rgb("green", true), 0x4ade80);
        assert_eq!(project_rgb("no-such-colour", true), project_rgb("gray", true));
    }
}
