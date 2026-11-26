//! Spacing, sizes, typography, and layout constants for Domovoi Desktop.

pub const XXS: f32 = 2.0;
pub const XS: f32 = 4.0;
pub const TIGHT: f32 = 6.0;
pub const S: f32 = 8.0;
pub const M: f32 = 12.0;
pub const L: f32 = 16.0;
pub const XL: f32 = 24.0;
pub const XXL: f32 = 32.0;

pub const ROW_PAD: [f32; 2] = [6.0, S];
pub const ROW_PAD_TALL: [f32; 2] = [8.0, S];
pub const BUTTON_PAD: [f32; 2] = [6.0, M];
pub const SMALL_BUTTON_PAD: [f32; 2] = [4.0, S];
pub const CHIP_PAD: [f32; 2] = [4.0, 10.0];
pub const INPUT_PAD: [f32; 2] = [8.0, M];
pub const INDENT: f32 = 14.0;
pub const ICON_GAP: f32 = 6.0;

pub const CARD_PAD: f32 = L;
pub const CARD_PAD_COMPACT: f32 = M;
pub const CHROME_PAD_X: f32 = M;
pub const CONTENT_GUTTER: f32 = XL;
pub const SECTION_GAP: f32 = M;
pub const SIDEBAR_ICON_COL: f32 = 24.0;
pub const SIDEBAR_STATUS_COL: f32 = 14.0;

pub const RADIUS_SMALL: f32 = 4.0;
pub const RADIUS_MEDIUM: f32 = 6.0;
pub const RADIUS_CHIP: f32 = 8.0;
pub const RADIUS_CARD: f32 = 12.0;
pub const RADIUS_BUBBLE: f32 = 16.0;
pub const RADIUS_PILL: f32 = 999.0;

pub const TEXT_CAPTION: f32 = 11.0;
pub const TEXT_SMALL: f32 = 12.0;
pub const TEXT_UI: f32 = 13.0;
pub const TEXT_BODY: f32 = 14.0;
pub const TEXT_CODE: f32 = 12.0;
pub const TEXT_TITLE: f32 = 18.0;

pub const ICON_XS: f32 = 12.0;
pub const ICON_SMALL: f32 = 14.0;
pub const ICON: f32 = 16.0;
pub const ICON_LARGE: f32 = 20.0;

/// Desktop layout geometry.
pub const TOP_BAR: f32 = 42.0;
pub const SUB_BAR_HEIGHT: f32 = 44.0;
pub const STATUS_BAR: f32 = 28.0;
pub const SIDEBAR_DEFAULT: f32 = 280.0;
pub const SIDEBAR_MIN: f32 = 210.0;
pub const SIDEBAR_MAX: f32 = 420.0;
pub const PANEL_DEFAULT: f32 = 440.0;
pub const PANEL_MIN: f32 = 280.0;
pub const PANEL_MAX: f32 = 820.0;
pub const TRANSCRIPT_MAX_WIDTH: f32 = 900.0;

/// User-adjustable UI/transcript text scale bounds.
pub const TEXT_DEFAULT: f32 = 14.0;
pub const TEXT_MIN: f32 = 12.0;
pub const TEXT_MAX: f32 = 18.0;
