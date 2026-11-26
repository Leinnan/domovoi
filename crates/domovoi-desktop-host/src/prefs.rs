//! What a desktop client remembers about its own window (appearance, text
//! size, pane widths): a small `key = value` file beside its store. Nothing
//! here reaches the server or the core.

use crate::Identity;

/// The transcript's text size, in points.
pub const TEXT_DEFAULT: f32 = 14.0;
pub const TEXT_MIN: f32 = 12.0;
pub const TEXT_MAX: f32 = 18.0;
/// The thread list's width.
pub const SIDEBAR_DEFAULT: f32 = 272.0;
pub const SIDEBAR_MIN: f32 = 210.0;
pub const SIDEBAR_MAX: f32 = 420.0;
/// The right panel's width.
pub const PANEL_DEFAULT: f32 = 440.0;
pub const PANEL_MIN: f32 = 280.0;
pub const PANEL_MAX: f32 = 820.0;

/// Light, dark, or whichever the system is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Where a window was: its restored bounds in screen pixels, and whether it was maximized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFrame {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl WindowFrame {
    /// `x,y,width,height,maximized`; `None` for anything that is not a window's bounds.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(',').map(str::trim);
        let mut number = || parts.next()?.parse::<i32>().ok();
        let (x, y, width, height) = (number()?, number()?, number()?, number()?);
        let maximized = parts.next()? == "true";
        let fits = width > 0 && height > 0 && x.checked_add(width).is_some() && y.checked_add(height).is_some();
        fits.then_some(Self { x, y, width, height, maximized })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub appearance: Appearance,
    pub text_size: f32,
    pub sidebar_width: f32,
    pub panel_width: f32,
    pub show_sidebar: bool,
    /// Where the window was when it was last moved or sized (`None`: never, so the default size).
    pub window: Option<WindowFrame>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { appearance: Appearance::System, text_size: TEXT_DEFAULT, sidebar_width: SIDEBAR_DEFAULT, panel_width: PANEL_DEFAULT, show_sidebar: true, window: None }
    }
}

impl Prefs {
    /// Reads the file's text. A line it does not know is skipped, and a value
    /// out of range is brought into it, so an older or newer client's file reads.
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim());
            let number = |default: f32, min: f32, max: f32| value.parse::<f32>().ok().filter(|n| n.is_finite()).unwrap_or(default).clamp(min, max);
            match key {
                "appearance" => prefs.appearance = Appearance::ALL.into_iter().find(|a| a.name() == value).unwrap_or_default(),
                "text_size" => prefs.text_size = number(prefs.text_size, TEXT_MIN, TEXT_MAX),
                "sidebar_width" => prefs.sidebar_width = number(prefs.sidebar_width, SIDEBAR_MIN, SIDEBAR_MAX),
                "panel_width" => prefs.panel_width = number(prefs.panel_width, PANEL_MIN, PANEL_MAX),
                "show_sidebar" => prefs.show_sidebar = value == "true",
                "window" => prefs.window = WindowFrame::parse(value),
                _ => {}
            }
        }
        prefs
    }

    pub fn serialize(&self) -> String {
        let mut text = format!(
            "appearance = {}\ntext_size = {}\nsidebar_width = {}\npanel_width = {}\nshow_sidebar = {}\n",
            self.appearance.name(),
            self.text_size,
            self.sidebar_width.round(),
            self.panel_width.round(),
            self.show_sidebar,
        );
        if let Some(w) = self.window {
            text.push_str(&format!("window = {},{},{},{},{}\n", w.x, w.y, w.width, w.height, w.maximized));
        }
        text
    }

    /// Where `identity`'s file is; `None` when nothing is to be kept (tests, `DOMOVOI_STORE=memory`).
    fn path(identity: Identity) -> Option<std::path::PathBuf> {
        if cfg!(test) || std::env::var("DOMOVOI_STORE").ok().as_deref() == Some("memory") {
            return None;
        }
        Some(crate::default_data_dir(identity).join("desktop-preferences.txt"))
    }

    pub fn load(identity: Identity) -> Self {
        Self::path(identity).and_then(|path| std::fs::read_to_string(path).ok()).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    pub fn save(&self, identity: Identity) {
        if let Some(path) = Self::path(identity) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(error) = std::fs::write(&path, self.serialize()) {
                tracing::warn!("the preferences could not be saved to {}: {error}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_saved_reads_back() {
        let window = Some(WindowFrame { x: -1200, y: 40, width: 1300, height: 900, maximized: true });
        let prefs = Prefs { appearance: Appearance::Dark, text_size: 16.0, sidebar_width: 300.0, panel_width: 500.0, show_sidebar: false, window };
        assert_eq!(Prefs::parse(&prefs.serialize()), prefs);
    }

    #[test]
    fn a_file_with_unknown_or_wild_lines_still_reads() {
        let prefs = Prefs::parse("appearance = sepia\ntext_size = 400\nwindow = 10,20\nnot a line\nsidebar_width = wide\nwindow = 0,0,-5,100,false\n");
        assert_eq!(prefs, Prefs { text_size: TEXT_MAX, ..Prefs::default() });
    }
}
