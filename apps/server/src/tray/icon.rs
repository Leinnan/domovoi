//! The status bar icon: Domovoi's keeper's lantern with a hearth flame inside
//! (the single-colour glyph from tools/icons.py), with a mark for the state,
//! drawn at start (no image assets). The lantern sits low and to the left so
//! the mark has the top-right corner. The glyph is black or white to match the
//! bar, with a green dot when a device is connected and a red exclamation mark
//! on an error. macOS gets a template image (which the system tints) while
//! there is no mark to colour.

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

enum Op {
    Line((f32, f32)),
    Cubic((f32, f32), (f32, f32), (f32, f32)),
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

    // Keeper's lantern monochrome glyph mirrored from tools/icons.py:
    // bail ring + tapered lantern frame with arched window cutout + hearth flame.
    // Mapped from the 1024 grid (centre 512, 484) onto the low-left of the
    // 44-unit grid (scale 0.046, centred at 16.5, 25.5) so the top-right stays
    // clear for the connected/error badge.
    const S: f32 = 0.046;
    let map = |(x, y): (f32, f32)| (16.5 + (x - 512.0) * S, 25.5 + (y - 484.0) * S);
    let trace = |builder: &mut PathBuilder, start: (f32, f32), ops: &[Op]| {
        let (x, y) = map(start);
        builder.move_to(x, y);
        for op in ops {
            match *op {
                Op::Line(p) => {
                    let (x, y) = map(p);
                    builder.line_to(x, y);
                }
                Op::Cubic(p1, p2, p3) => {
                    let ((x1, y1), (x2, y2), (x3, y3)) = (map(p1), map(p2), map(p3));
                    builder.cubic_to(x1, y1, x2, y2, x3, y3);
                }
            }
        }
        builder.close();
    };

    // 1. Top bail ring (filled even-odd separately so the ring/cap overlap never cancels out).
    let mut ring = PathBuilder::new();
    for radius in [56.0_f32, 22.0] {
        let (cx, cy) = map((512.0, 186.0));
        if let Some(oval) = tiny_skia::Rect::from_xywh(cx - radius * S, cy - radius * S, 2.0 * radius * S, 2.0 * radius * S) {
            ring.push_oval(oval);
        }
    }
    if let Some(path) = ring.finish() {
        pixmap.fill_path(&path, &glyph, FillRule::EvenOdd, scale, None);
    }

    // 2. Tapered lantern body with arched window cut out by the even-odd rule.
    let mut frame = PathBuilder::new();
    trace(&mut frame, (457.2, 248.4), &[
        Op::Cubic((487.5, 234.9), (536.5, 234.9), (566.8, 248.4)),
        Op::Line((761.9, 335.1)),
        Op::Cubic((773.0, 340.0), (773.7, 349.4), (763.6, 356.0)),
        Op::Line((752.1, 363.5)),
        Op::Cubic((746.5, 367.1), (741.5, 375.3), (740.9, 381.9)),
        Op::Line((707.1, 744.1)),
        Op::Cubic((706.5, 750.7), (710.8, 758.5), (716.6, 761.6)),
        Op::Line((732.1, 769.6)),
        Op::Cubic((740.9, 774.3), (748.0, 786.1), (748.0, 796.0)),
        Op::Line((748.0, 810.0)),
        Op::Cubic((748.0, 825.5), (735.5, 838.0), (720.0, 838.0)),
        Op::Line((304.0, 838.0)),
        Op::Cubic((288.5, 838.0), (276.0, 825.5), (276.0, 810.0)),
        Op::Line((276.0, 796.0)),
        Op::Cubic((276.0, 786.1), (283.1, 774.3), (291.9, 769.6)),
        Op::Line((307.4, 761.6)),
        Op::Cubic((313.2, 758.5), (317.5, 750.7), (316.9, 744.1)),
        Op::Line((283.1, 381.9)),
        Op::Cubic((282.5, 375.3), (277.5, 367.1), (271.9, 363.5)),
        Op::Line((260.4, 356.0)),
        Op::Cubic((250.3, 349.4), (251.0, 340.0), (262.1, 335.1)),
    ]);
    // Arched window cutout (MONO_WINDOW_PATH).
    trace(&mut frame, (512.0, 394.0), &[
        Op::Cubic((597.1, 394.0), (684.0, 462.9), (684.0, 548.0)),
        Op::Line((660.0, 720.0)),
        Op::Cubic((660.0, 733.3), (649.3, 744.0), (636.0, 744.0)),
        Op::Line((388.0, 744.0)),
        Op::Cubic((374.7, 744.0), (364.0, 733.3), (364.0, 720.0)),
        Op::Line((340.0, 548.0)),
        Op::Cubic((340.0, 462.9), (426.9, 394.0), (512.0, 394.0)),
    ]);
    if let Some(path) = frame.finish() {
        pixmap.fill_path(&path, &glyph, FillRule::EvenOdd, scale, None);
    }

    // 3. Solid hearth flame inside the lantern window (MONO_FLAME_PATH).
    let mut flame = PathBuilder::new();
    trace(&mut flame, (512.0, 476.0), &[
        Op::Cubic((530.2, 522.8), (588.0, 569.6), (588.0, 623.4)),
        Op::Cubic((588.0, 698.3), (553.0, 710.0), (512.0, 710.0)),
        Op::Cubic((471.0, 710.0), (436.0, 698.3), (436.0, 623.4)),
        Op::Cubic((436.0, 569.6), (493.8, 522.8), (512.0, 476.0)),
    ]);
    if let Some(path) = flame.finish() {
        pixmap.fill_path(&path, &glyph, FillRule::Winding, scale, None);
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
