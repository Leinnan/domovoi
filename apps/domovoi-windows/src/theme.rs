//! The few sizes and brushes that set the window's areas apart: the window
//! is the Mica backdrop, and what holds content sits on it as a card.

use windows_reactor::{Brush, Color, TextBlock, ThemeBrush, VerticalAlignment};

/// A card's corner radius.
pub const RADIUS: f64 = 8.0;
/// The space between cards, and between them and the window's edge.
pub const GAP: f64 = 8.0;
/// The sidebar's width.
pub const SIDEBAR: f64 = 280.0;
/// Body text, secondary text and headings.
pub const BODY: f64 = 14.0;
pub const CAPTION: f64 = 12.0;
pub const TITLE: f64 = 20.0;
/// Code and command output.
pub const MONO: &str = "Cascadia Mono, Consolas";

pub fn card() -> Brush {
    ThemeBrush::CardBackground.into()
}

pub fn stroke() -> Brush {
    ThemeBrush::CardStroke.into()
}

/// The link dialog's panel. Opaque: a card's fill lets what is under it show
/// through, and under the dialog that is the dimmed window.
pub fn panel() -> Brush {
    ThemeBrush::SolidBackground.into()
}

/// The shade over the window while the link dialog asks.
pub fn scrim() -> Brush {
    Color::argb(0x4D, 0x00, 0x00, 0x00).into()
}

/// A code block's fill, a shade off its card.
pub fn code() -> Brush {
    Color::argb(0x14, 0x80, 0x80, 0x80).into()
}

/// The selected row's fill.
pub fn selected() -> Brush {
    Color::argb(0x26, 0x80, 0x80, 0x80).into()
}

/// No fill at all (a row that is not the selected one).
pub fn clear() -> Brush {
    Color::argb(0, 0, 0, 0).into()
}

/// The window's own colour, opaque (behind a badge on an icon).
pub fn solid() -> Brush {
    ThemeBrush::SolidBackground.into()
}

/// The colour text is in: a ring that stands out on any swatch.
pub fn primary() -> Brush {
    ThemeBrush::PrimaryText.into()
}

pub fn accent() -> Brush {
    ThemeBrush::AccentText.into()
}

pub fn critical() -> Brush {
    ThemeBrush::SystemCritical.into()
}

/// An opaque `0xRRGGBB` colour (a project's, from the shared palette).
pub fn rgb(hex: u32) -> Brush {
    Color::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8).into()
}

/// The fill of a tile holding something drawn in `hex`: that colour, faint
/// (a little stronger on a dark window, as the other clients have it).
pub fn tint(hex: u32, dark: bool) -> Brush {
    Color::argb(if dark { 0x29 } else { 0x1C }, (hex >> 16) as u8, (hex >> 8) as u8, hex as u8).into()
}

/// A Segoe Fluent Icons glyph.
pub fn glyph(code: &str, size: f64) -> TextBlock {
    TextBlock::new().text(code).font_family("Segoe Fluent Icons, Segoe MDL2 Assets").font_size(size).vertical_alignment(VerticalAlignment::Center)
}
