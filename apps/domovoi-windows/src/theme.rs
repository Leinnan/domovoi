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

/// A code block's fill, a shade off its card.
pub fn code() -> Brush {
    Color::argb(0x14, 0x80, 0x80, 0x80).into()
}

/// The selected row's fill.
pub fn selected() -> Brush {
    Color::argb(0x26, 0x80, 0x80, 0x80).into()
}

pub fn accent() -> Brush {
    ThemeBrush::AccentText.into()
}

pub fn critical() -> Brush {
    ThemeBrush::SystemCritical.into()
}

/// A Segoe Fluent Icons glyph.
pub fn glyph(code: &str, size: f64) -> TextBlock {
    TextBlock::new().text(code).font_family("Segoe Fluent Icons, Segoe MDL2 Assets").font_size(size).vertical_alignment(VerticalAlignment::Center)
}
