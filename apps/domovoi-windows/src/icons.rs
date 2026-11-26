//! The symbols a project icon can be: the Lucide drawings the Iced app
//! ships, coloured for the tile they sit on.

use std::cell::RefCell;
use std::collections::HashMap;

use windows_reactor::EncodedImage;

/// The drawing for a project symbol's name (`ProjectIconView::symbol`) or
/// one of the window's own; a folder for a name this app has no drawing of.
fn drawing(symbol: &str) -> &'static str {
    macro_rules! svg {
        ($file:literal) => {
            include_str!(concat!("../../domovoi-iced/assets/icons/", $file, ".svg"))
        };
    }
    match symbol {
        "folder-code" => svg!("folder-open"),
        "code-2" => svg!("code"),
        "terminal" => svg!("terminal"),
        "globe-2" => svg!("globe"),
        "server" => svg!("server"),
        "database" => svg!("database"),
        "bot" => svg!("bot"),
        "sparkles" => svg!("sparkles"),
        "smartphone" => svg!("smartphone"),
        "monitor" => svg!("monitor"),
        "cloud-cog" => svg!("cpu"),
        "package" => svg!("package"),
        "book-open" => svg!("book-open"),
        "flask-conical" => svg!("flask-conical"),
        "shield-check" => svg!("shield"),
        "rocket" => svg!("rocket"),
        "gamepad-2" => svg!("gamepad-2"),
        "music" => svg!("music"),
        "image" => svg!("image"),
        "shopping-bag" => svg!("shopping-bag"),
        "git-branch" => svg!("git-branch"),
        "workflow" => svg!("waypoints"),
        "wrench" => svg!("wrench"),
        "layers-3" => svg!("layers"),
        "message-square-dashed" => svg!("message-square"),
        // Not project symbols: drawings the window itself uses (`widgets::line_icon`).
        "diff" => svg!("diff"),
        "git-pull-request" => svg!("git-pull-request"),
        "bar-chart" => svg!("bar-chart-2"),
        _ => svg!("folder"),
    }
}

/// The symbols the icon picker offers, as the core and the other clients name them.
pub const SYMBOLS: [&str; 24] = [
    "folder-code", "code-2", "terminal", "globe-2", "server", "database", "bot", "sparkles", "smartphone", "monitor", "cloud-cog", "package", "book-open",
    "flask-conical", "shield-check", "rocket", "gamepad-2", "music", "image", "shopping-bag", "git-branch", "workflow", "wrench", "layers-3",
];

/// `svg` with its strokes in `rgb` (the drawings stroke in the current
/// colour or in black).
fn tinted(svg: &str, rgb: u32) -> String {
    let colour = format!("#{rgb:06x}");
    svg.replace("currentColor", &colour).replace("#000000", &colour)
}

thread_local! {
    /// One image per symbol and colour: the same bytes each render, so the
    /// toolkit decodes each once.
    static TINTED: RefCell<HashMap<(String, u32), EncodedImage>> = RefCell::new(HashMap::new());
}

/// The symbol named `symbol`, drawn in `rgb`.
pub fn symbol(symbol: &str, rgb: u32) -> EncodedImage {
    TINTED.with(|cache| {
        cache.borrow_mut().entry((symbol.to_owned(), rgb)).or_insert_with(|| EncodedImage::new(tinted(drawing(symbol), rgb).into_bytes())).clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drawing_takes_the_tile_colour_whichever_way_it_strokes() {
        for name in ["rocket", "folder-code", "no-such-symbol"] {
            let svg = tinted(drawing(name), 0x4ade80);
            assert!(svg.contains("stroke=\"#4ade80\""), "{name}: {svg}");
            assert!(!svg.contains("currentColor") && !svg.contains("#000000"), "{name}");
        }
    }

    #[test]
    fn a_symbol_and_colour_is_one_image() {
        assert_eq!(symbol("rocket", 0x112233), symbol("rocket", 0x112233));
        assert_ne!(symbol("rocket", 0x112233), symbol("rocket", 0x445566));
    }
}
