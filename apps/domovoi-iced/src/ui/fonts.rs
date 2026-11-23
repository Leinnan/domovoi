//! Bundled OFL fonts: Inter for UI & prose, JetBrains Mono for code & terminals.

use iced::Font;
use iced::font::{Style, Weight};

pub const INTER: Font = Font::with_name("Inter");
pub const MEDIUM: Font = Font { weight: Weight::Medium, ..INTER };
pub const SEMIBOLD: Font = Font { weight: Weight::Semibold, ..INTER };
pub const BOLD: Font = Font { weight: Weight::Bold, ..INTER };
pub const ITALIC: Font = Font { style: Style::Italic, ..INTER };
pub const BOLD_ITALIC: Font = Font { weight: Weight::Bold, style: Style::Italic, ..INTER };
pub const MONO: Font = Font::with_name("JetBrains Mono");
pub const MONO_BOLD: Font = Font { weight: Weight::Bold, ..MONO };
pub const MONO_ITALIC: Font = Font { style: Style::Italic, ..MONO };

pub const FILES: [&[u8]; 8] = [
    include_bytes!("../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../../assets/fonts/Inter-BoldItalic.ttf"),
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf"),
];

pub fn settings() -> iced::Settings {
    iced::Settings {
        fonts: FILES.iter().map(|f| std::borrow::Cow::Borrowed(*f)).collect(),
        default_font: INTER,
        default_text_size: iced::Pixels(13.0),
        antialiasing: true,
        ..Default::default()
    }
}

pub fn styled(bold: bool, italic: bool) -> Font {
    match (bold, italic) {
        (true, true) => BOLD_ITALIC,
        (true, false) => BOLD,
        (false, true) => ITALIC,
        (false, false) => INTER,
    }
}
