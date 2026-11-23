//! Domovoi's "Hearth" palette as Iced themes: night-indigo/slate dark surfaces
//! and crisp warm-neutral light surfaces paired with luminous ember amber.

use iced::theme::palette::{Background, Extended, Pair, Palette, Primary, Secondary};
use iced::{Color, Theme, color};

/// Semantic colours of one appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hearth {
    pub dark: bool,
    pub sunken: Color,
    pub bg: Color,
    pub surface: Color,
    pub raised: Color,
    pub border: Color,
    pub border_strong: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub on_accent: Color,
    pub accent_soft: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub violet: Color,
    pub highlight: Color,
    pub favorite: Color,
    // Agent provider accents.
    pub claude: Color,
    pub codex: Color,
    pub pi: Color,
    pub antigravity: Color,
    // Syntax highlighting.
    pub code_keyword: Color,
    pub code_string: Color,
    pub code_comment: Color,
    pub code_number: Color,
    pub code_type: Color,
    pub code_function: Color,
}

pub const DARK: Hearth = Hearth {
    dark: true,
    sunken: color!(0x0E1017),
    bg: color!(0x13161F),
    surface: color!(0x1A1E29),
    raised: color!(0x222736),
    border: color!(0x30374A),
    border_strong: color!(0x444D66),
    text: color!(0xECEAE4),
    muted: color!(0x9DA5BA),
    faint: color!(0x6F778D),
    accent: color!(0xF29932),
    accent_hover: color!(0xFFAF4D),
    on_accent: color!(0x140F0A),
    accent_soft: color!(0xF29932, 0.16),
    success: color!(0x6EC888),
    warning: color!(0xEAB949),
    danger: color!(0xF07167),
    info: color!(0x6EB5E8),
    violet: color!(0xB79CED),
    highlight: color!(0xEAB949),
    favorite: color!(0xF2C94C),
    claude: color!(0xF29932),
    codex: color!(0x4FD1C5),
    pi: color!(0xF48FB1),
    antigravity: color!(0x9FA8DA),
    code_keyword: color!(0xF48FB1),
    code_string: color!(0x6EC888),
    code_comment: color!(0x7A8299),
    code_number: color!(0x6EB5E8),
    code_type: color!(0x4FD1C5),
    code_function: color!(0xE6C07B),
};

pub const LIGHT: Hearth = Hearth {
    dark: false,
    sunken: color!(0xE7E9F0),
    bg: color!(0xFAF9F7),
    surface: color!(0xF1F2F6),
    raised: color!(0xFFFFFF),
    border: color!(0xD8DCE6),
    border_strong: color!(0xBCC3D4),
    text: color!(0x1B1F2A),
    muted: color!(0x525B70),
    faint: color!(0x828B9E),
    accent: color!(0xAD5608),
    accent_hover: color!(0x914605),
    on_accent: color!(0xFFFDF9),
    accent_soft: color!(0xAD5608, 0.12),
    success: color!(0x267340),
    warning: color!(0x8C5E00),
    danger: color!(0xC03227),
    info: color!(0x1F6396),
    violet: color!(0x6B46B0),
    highlight: color!(0xA16207),
    favorite: color!(0xB45309),
    claude: color!(0xAD5608),
    codex: color!(0x0F766E),
    pi: color!(0xBE185D),
    antigravity: color!(0x4338CA),
    code_keyword: color!(0xBE185D),
    code_string: color!(0x267340),
    code_comment: color!(0x6B7280),
    code_number: color!(0x1F6396),
    code_type: color!(0x0F766E),
    code_function: color!(0x6B46B0),
};

/// The user's appearance choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Appearance::System => system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        }
    }
}

pub fn colors(theme: &Theme) -> &'static Hearth {
    if theme.extended_palette().is_dark { &DARK } else { &LIGHT }
}

pub fn theme(dark: bool) -> Theme {
    let h = if dark { DARK } else { LIGHT };
    let palette = Palette {
        background: h.bg,
        text: h.text,
        primary: h.accent,
        success: h.success,
        warning: h.warning,
        danger: h.danger,
    };
    Theme::custom_with_fn(
        std::borrow::Cow::Borrowed(if dark { "Hearth Dark" } else { "Hearth Light" }),
        palette,
        move |p| {
            let mut e = Extended::generate(p);
            e.is_dark = dark;
            e.background = Background {
                base: Pair::new(h.bg, h.text),
                weakest: Pair::new(h.surface, h.text),
                weaker: Pair::new(h.surface, h.text),
                weak: Pair::new(h.raised, h.text),
                neutral: Pair::new(h.border, h.text),
                strong: Pair::new(h.border_strong, h.text),
                stronger: Pair::new(h.faint, h.text),
                strongest: Pair::new(h.muted, h.text),
            };
            e.primary = Primary {
                base: Pair { color: h.accent, text: h.on_accent },
                weak: Pair { color: h.accent_soft, text: h.text },
                strong: Pair { color: h.accent_hover, text: h.on_accent },
            };
            e.secondary = Secondary {
                base: Pair { color: h.raised, text: h.text },
                weak: Pair { color: h.surface, text: h.text },
                strong: Pair { color: h.border, text: h.text },
            };
            e
        },
    )
}

pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

pub fn agent_color(name: &str, h: &Hearth) -> Color {
    let lower = name.to_ascii_lowercase();
    if lower.contains("codex") || lower.contains("openai") {
        h.codex
    } else if lower.contains("pi") {
        h.pi
    } else if lower.contains("antigravity") || lower.contains("gemini") {
        h.antigravity
    } else if lower.contains("claude") || lower.contains("anthropic") {
        h.claude
    } else {
        h.accent
    }
}

pub fn code_span_color(kind: &str, h: &Hearth) -> Option<Color> {
    match kind {
        "keyword" => Some(h.code_keyword),
        "string" => Some(h.code_string),
        "comment" => Some(h.code_comment),
        "number" => Some(h.code_number),
        "type" => Some(h.code_type),
        "function" => Some(h.code_function),
        "added" => Some(h.success),
        "removed" => Some(h.danger),
        _ => None,
    }
}

/// Shared 18-colour project & companion palette (matches `ProjectIcon.swift`).
pub const PROJECT_KEYS: [&str; 18] = [
    "gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan", "sky", "blue", "indigo", "violet", "purple",
    "fuchsia", "pink", "rose",
];

const PROJECT_LIGHT: [u32; 18] = [
    0x4b5563, 0x991b1b, 0x9a3412, 0x92400e, 0x854d0e, 0x3f6212, 0x166534, 0x065f46, 0x115e59, 0x155e75, 0x075985, 0x1e40af, 0x4338ca,
    0x6d28d9, 0x7e22ce, 0xa21caf, 0x9d174d, 0x9f1239,
];

const PROJECT_DARK: [u32; 18] = [
    0x9ca3af, 0xfca5a5, 0xfb923c, 0xfbbf24, 0xfacc15, 0xa3e635, 0x4ade80, 0x34d399, 0x2dd4bf, 0x22d3ee, 0x38bdf8, 0x60a5fa, 0xa5b4fc,
    0xc4b5fd, 0xd8b4fe, 0xe879f9, 0xf9a8d4, 0xfda4af,
];

pub fn project_color(key: &str, dark: bool) -> Color {
    let idx = PROJECT_KEYS.iter().position(|&k| k == key).unwrap_or(0);
    let hex = if dark { PROJECT_DARK[idx] } else { PROJECT_LIGHT[idx] };
    rgb_u32(hex)
}

pub fn rgb_u32(hex: u32) -> Color {
    Color::from_rgb8(((hex >> 16) & 0xFF) as u8, ((hex >> 8) & 0xFF) as u8, (hex & 0xFF) as u8)
}

/// xterm 256-color palette + 24-bit truecolor (matches `TerminalColors.swift`).
pub fn xterm_color(value: i32) -> Option<Color> {
    if value < 0 {
        return None;
    }
    if value >= 0x100_0000 {
        return Some(rgb_u32((value & 0xFF_FFFF) as u32));
    }
    const ANSI: [(f32, f32, f32); 16] = [
        (0.0, 0.0, 0.0),
        (0.80, 0.19, 0.19),
        (0.05, 0.74, 0.47),
        (0.90, 0.90, 0.06),
        (0.14, 0.45, 0.78),
        (0.74, 0.25, 0.74),
        (0.07, 0.66, 0.80),
        (0.90, 0.90, 0.90),
        (0.40, 0.40, 0.40),
        (0.95, 0.30, 0.30),
        (0.14, 0.82, 0.55),
        (0.96, 0.96, 0.26),
        (0.23, 0.56, 0.92),
        (0.84, 0.44, 0.84),
        (0.16, 0.72, 0.86),
        (1.0, 1.0, 1.0),
    ];
    let i = value as usize;
    if i < 16 {
        let (r, g, b) = ANSI[i];
        return Some(Color::from_rgb(r, g, b));
    }
    if i < 232 {
        let n = i - 16;
        let level = |v: usize| if v == 0 { 0.0 } else { (v as f32 * 40.0 + 55.0) / 255.0 };
        return Some(Color::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6)));
    }
    if i < 256 {
        let gray = ((i - 232) as f32 * 10.0 + 8.0) / 255.0;
        return Some(Color::from_rgb(gray, gray, gray));
    }
    None
}

#[cfg(test)]
pub fn luminance(c: Color) -> f32 {
    let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}

#[cfg(test)]
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hearth_tones_meet_wcag_contrast_on_every_surface() {
        for h in [DARK, LIGHT] {
            for (name, surface) in [("bg", h.bg), ("surface", h.surface), ("raised", h.raised), ("sunken", h.sunken)] {
                assert!(contrast(h.text, surface) >= 7.0, "text on {name} (dark={}): {}", h.dark, contrast(h.text, surface));
                assert!(contrast(h.muted, surface) >= 4.5, "muted on {name} (dark={}): {}", h.dark, contrast(h.muted, surface));
                assert!(contrast(h.accent, surface) >= 3.0, "accent on {name} (dark={}): {}", h.dark, contrast(h.accent, surface));
                for (k, c) in [("success", h.success), ("warning", h.warning), ("danger", h.danger), ("info", h.info), ("violet", h.violet)] {
                    assert!(contrast(c, surface) >= 3.0, "{k} on {name} (dark={}): {}", h.dark, contrast(c, surface));
                }
            }
            assert!(contrast(h.on_accent, h.accent) >= 4.5, "on_accent (dark={}): {}", h.dark, contrast(h.on_accent, h.accent));
        }
    }

    #[test]
    fn xterm_palette_decodes_ansi_cube_gray_and_truecolor() {
        assert_eq!(xterm_color(-1), None);
        assert!(xterm_color(1).is_some());
        assert!(xterm_color(196).is_some());
        assert!(xterm_color(244).is_some());
        let rgb = xterm_color(0x100_0000 | 0x336699).unwrap();
        assert!((rgb.r - 0x33 as f32 / 255.0).abs() < 0.01);
        assert!((rgb.g - 0x66 as f32 / 255.0).abs() < 0.01);
        assert!((rgb.b - 0x99 as f32 / 255.0).abs() < 0.01);
    }
}
