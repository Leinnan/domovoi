//! The status bar icon: the domovoi's peaked Hearth Hood and two ember eyes
//! (the app icon's silhouette from tools/icons.py) with a mark for the state,
//! drawn at start (no image assets). The face sits low and to the left so the
//! mark has the top-right corner. The glyph is black or white to match the bar,
//! with a green dot when a device is connected and a red exclamation mark on an
//! error. macOS gets a template image (which the system tints) while there is
//! no mark to colour.

use super::model::IconKind;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    /// Black with alpha only (macOS template image).
    Template,
    /// For a light bar: black glyph.
    OnLight,
    /// For a dark bar: white glyph.
    OnDark,
}

/// RGBA pixels of a `size`×`size` icon.
pub fn render(kind: IconKind, style: Style, size: u32) -> Vec<u8> {
    let mut pixmap = Pixmap::new(size, size).expect("a non-empty icon");
    // Drawn on a 44-unit grid.
    let scale = Transform::from_scale(size as f32 / 44.0, size as f32 / 44.0);
    let (r, g, b) = if style == Style::OnDark { (255, 255, 255) } else { (0, 0, 0) };
    // Off: the same glyph, faded.
    let glyph_alpha = if kind == IconKind::Off { 115 } else { 255 };
    let mut glyph = Paint::default();
    glyph.set_color_rgba8(r, g, b, glyph_alpha);
    glyph.anti_alias = true;

    // The sculpted peaked Hearth Hood (same silhouette as tools/icons.py),
    // mapped onto the low-left of the 44-unit grid (scale 0.0455, centered at
    // x = 18.0) so the top-right stays clear for the connected/error badge.
    let map = |x: f32, y: f32| (18.0 + (x - 512.0) * 0.0455, 25.0 + (y - 472.0) * 0.0455);
    let mut hood = PathBuilder::new();
    let (x, y) = map(512.0, 204.0);
    hood.move_to(x, y);
    for (p1, p2, p3) in [
        ((554.0, 204.0), (636.0, 254.0), (718.0, 352.0)),
        ((792.0, 440.0), (844.0, 552.0), (858.0, 666.0)),
        ((864.0, 708.0), (832.0, 742.0), (792.0, 738.0)),
        ((758.0, 734.0), (738.0, 710.0), (730.0, 672.0)),
        ((714.0, 596.0), (682.0, 518.0), (634.0, 452.0)),
        ((590.0, 392.0), (544.0, 358.0), (512.0, 358.0)),
        ((480.0, 358.0), (434.0, 392.0), (390.0, 452.0)),
        ((342.0, 518.0), (310.0, 596.0), (294.0, 672.0)),
        ((286.0, 710.0), (266.0, 734.0), (232.0, 738.0)),
        ((192.0, 742.0), (160.0, 708.0), (166.0, 666.0)),
        ((180.0, 552.0), (232.0, 440.0), (306.0, 352.0)),
        ((388.0, 254.0), (470.0, 204.0), (512.0, 204.0)),
    ] {
        let (x1, y1) = map(p1.0, p1.1);
        let (x2, y2) = map(p2.0, p2.1);
        let (x3, y3) = map(p3.0, p3.1);
        hood.cubic_to(x1, y1, x2, y2, x3, y3);
    }
    hood.close();
    if let Some(path) = hood.finish() {
        pixmap.fill_path(&path, &glyph, FillRule::Winding, scale, None);
    }
    // Two soft vertical-oval ember eyes matching the app icon's proportions.
    for (cx_src, cy_src) in [(404.0_f32, 622.0_f32), (620.0, 622.0)] {
        let (cx, cy) = map(cx_src, cy_src);
        if let Some(eye) = tiny_skia::Rect::from_xywh(cx - 2.9, cy - 3.6, 5.8, 7.2).and_then(PathBuilder::from_oval) {
            pixmap.fill_path(&eye, &glyph, FillRule::Winding, scale, None);
        }
    }

    let mut mark = Paint::default();
    mark.anti_alias = true;
    match kind {
        IconKind::Connected => {
            // A filled dot: green where colour is allowed.
            mark.set_color(if style == Style::Template { Color::BLACK } else { Color::from_rgba8(52, 199, 89, 255) });
            if let Some(dot) = PathBuilder::from_circle(35.0, 11.0, 6.5) {
                pixmap.fill_path(&dot, &mark, FillRule::Winding, scale, None);
            }
        }
        IconKind::Error => {
            // An exclamation mark: red where colour is allowed.
            mark.set_color(if style == Style::Template { Color::BLACK } else { Color::from_rgba8(255, 69, 58, 255) });
            let mut bang = PathBuilder::new();
            bang.move_to(38.5, 3.5);
            bang.line_to(38.5, 11.5);
            if let Some(path) = bang.finish() {
                pixmap.stroke_path(&path, &mark, &Stroke { width: 4.2, line_cap: LineCap::Round, ..Default::default() }, scale, None);
            }
            if let Some(dot) = PathBuilder::from_circle(38.5, 17.5, 2.4) {
                pixmap.fill_path(&dot, &mark, FillRule::Winding, scale, None);
            }
        }
        IconKind::On | IconKind::Off => {}
    }
    // tiny-skia keeps premultiplied pixels; icons want straight alpha.
    pixmap.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }).collect()
}

/// A QR code as a PNG: black modules on white, with a quiet zone.
pub fn qr_png(text: &str) -> Option<Vec<u8>> {
    const MODULE: u32 = 10;
    const QUIET: u32 = 4;
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width() as u32;
    let size = (width + 2 * QUIET) * MODULE;
    let mut pixmap = Pixmap::new(size, size)?;
    pixmap.fill(Color::WHITE);
    let mut black = Paint::default();
    black.set_color(Color::BLACK);
    black.anti_alias = false;
    for (i, colour) in code.to_colors().into_iter().enumerate() {
        if colour == qrcode::Color::Dark {
            let (x, y) = (i as u32 % width, i as u32 / width);
            let rect = tiny_skia::Rect::from_xywh(((x + QUIET) * MODULE) as f32, ((y + QUIET) * MODULE) as f32, MODULE as f32, MODULE as f32)?;
            pixmap.fill_rect(rect, &black, Transform::identity(), None);
        }
    }
    pixmap.encode_png().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(pixels: &[u8]) -> usize {
        pixels.chunks(4).filter(|p| p[3] > 200).count()
    }

    #[test]
    fn each_state_draws_something_different() {
        let kinds = [IconKind::Off, IconKind::On, IconKind::Connected, IconKind::Error];
        let icons: Vec<Vec<u8>> = kinds.iter().map(|k| render(*k, Style::Template, 44)).collect();
        assert!(icons.iter().all(|i| i.len() == 44 * 44 * 4));
        for a in 0..icons.len() {
            for b in a + 1..icons.len() {
                assert_ne!(icons[a], icons[b], "{:?} and {:?} look the same", kinds[a], kinds[b]);
            }
        }
        // Off is the faded glyph: nothing fully opaque; On has solid strokes.
        assert_eq!(opaque(&icons[0]), 0);
        assert!(opaque(&icons[1]) > 150);
        assert!(opaque(&icons[2]) > opaque(&icons[1]), "the dot adds pixels");
        // A template image is black with alpha only.
        assert!(icons[2].chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0));
    }

    #[test]
    fn themes_and_sizes() {
        let dark = render(IconKind::Connected, Style::OnDark, 64);
        assert_eq!(dark.len(), 64 * 64 * 4);
        assert!(dark.chunks(4).any(|p| p[3] > 200 && p[0] > 240 && p[1] > 240 && p[2] > 240), "a white glyph");
        assert!(dark.chunks(4).any(|p| p[3] > 200 && p[1] > 150 && p[0] < 120), "a green dot");
        let light = render(IconKind::Error, Style::OnLight, 32);
        assert!(light.chunks(4).any(|p| p[3] > 200 && p[0] > 200 && p[1] < 120), "a red mark");
    }

    #[test]
    fn qr_codes_are_png_images() {
        let png = qr_png("domovoi://connect?ticket=endpointabc&code=ABCD-EFGH&name=Mac").unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = Pixmap::decode_png(&png).unwrap();
        assert!(decoded.width() == decoded.height() && decoded.width() >= 290);
        // The quiet zone is white, the finder pattern's corner black.
        assert_eq!(decoded.pixel(5, 5).unwrap().red(), 255);
        assert_eq!(decoded.pixel(45, 45).unwrap().red(), 0);
    }
}
