//! Desktop-only preferences (appearance, text size, pane widths), stored as a
//! small `key = value` file next to the desktop store.

use super::theme::Appearance;
use super::tokens;

#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub appearance: Appearance,
    pub text_size: f32,
    pub sidebar_width: f32,
    pub panel_width: f32,
    pub show_sidebar: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            text_size: tokens::TEXT_DEFAULT,
            sidebar_width: tokens::SIDEBAR_DEFAULT,
            panel_width: tokens::PANEL_DEFAULT,
            show_sidebar: true,
        }
    }
}

impl Prefs {
    pub fn parse(s: &str) -> Self {
        let mut p = Self::default();
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let num = || v.parse::<f32>().ok().filter(|n| n.is_finite());
            let flag = || v == "true";
            match k {
                "appearance" => {
                    p.appearance = match v {
                        "light" => Appearance::Light,
                        "dark" => Appearance::Dark,
                        _ => Appearance::System,
                    };
                }
                "text_size" => p.text_size = num().unwrap_or(p.text_size).clamp(tokens::TEXT_MIN, tokens::TEXT_MAX),
                "sidebar_width" => {
                    p.sidebar_width = num().unwrap_or(p.sidebar_width).clamp(tokens::SIDEBAR_MIN, tokens::SIDEBAR_MAX);
                }
                "panel_width" => p.panel_width = num().unwrap_or(p.panel_width).clamp(tokens::PANEL_MIN, tokens::PANEL_MAX),
                "show_sidebar" => p.show_sidebar = flag(),
                _ => {}
            }
        }
        p
    }

    pub fn serialize(&self) -> String {
        let appearance = match self.appearance {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        };
        format!(
            "appearance = {appearance}\ntext_size = {}\nsidebar_width = {}\npanel_width = {}\nshow_sidebar = {}\n",
            self.text_size,
            self.sidebar_width.round(),
            self.panel_width.round(),
            self.show_sidebar,
        )
    }

    fn path() -> Option<std::path::PathBuf> {
        if cfg!(test) || std::env::var("DOMOVOI_STORE").ok().as_deref() == Some("memory") {
            return None;
        }
        Some(crate::store::default_data_dir().join("desktop-preferences.txt"))
    }

    pub fn load() -> Self {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| Self::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, self.serialize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_clamps() {
        let p = Prefs {
            appearance: Appearance::Dark,
            text_size: 16.0,
            sidebar_width: 310.0,
            panel_width: 520.0,
            show_sidebar: false,
        };
        assert_eq!(Prefs::parse(&p.serialize()), p);
        let odd = Prefs::parse("text_size = 99\nsidebar_width = 12\npanel_width = 5000\nnonsense\n");
        assert_eq!(
            (odd.text_size, odd.sidebar_width, odd.panel_width),
            (tokens::TEXT_MAX, tokens::SIDEBAR_MIN, tokens::PANEL_MAX)
        );
    }
}
