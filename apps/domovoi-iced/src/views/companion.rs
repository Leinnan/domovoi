//! Domovoi Companion mascot rendered with Iced Canvas, faithfully porting
//! `CompanionPainter.swift` (`CompanionLook` + `ActivityView` mood, blink, gaze).

use domovoi_core::companion::CompanionLook;
use domovoi_core::view::ActivityView;
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

use crate::ui::theme::{alpha, colors, project_color};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Offline,
    Attention,
    Working,
    Finished,
    Failed,
    Idle,
}

/// The mascot's mood: the server's activity, with a brief `reaction` to a
/// turn that just finished or failed (see `Reaction`).
pub fn mood_for(connected: bool, activity: &ActivityView, reaction: Option<Mood>) -> Mood {
    if !connected {
        Mood::Offline
    } else if activity.attention > 0 {
        Mood::Attention
    } else if activity.working > 0 {
        Mood::Working
    } else {
        reaction.unwrap_or(Mood::Idle)
    }
}

/// A short reaction when a turn ends: happy for 1.2 s, a wince for 1.5 s
/// (a wince isn't cut short by a "finished" right after), like Apple's
/// `CompanionBrain`. Only a new beat on the same server reacts, so connecting
/// or switching servers stays quiet.
#[derive(Debug, Default)]
pub struct Reaction {
    server: String,
    seq: u64,
    pub current: Option<(Mood, i64)>,
}

impl Reaction {
    /// After a render: the beat to react to, and when the reaction ends (Unix ms).
    pub fn observe(&mut self, server: &str, activity: &ActivityView, now_ms: i64) -> Option<i64> {
        let fresh = self.server == server && self.seq != activity.beat_seq && self.seq != 0;
        let first = self.seq == 0 || self.server != server;
        self.server = server.to_owned();
        self.seq = activity.beat_seq;
        if first || !fresh {
            return None;
        }
        let wincing = matches!(self.current, Some((Mood::Failed, until)) if until > now_ms);
        let (mood, ms) = match activity.beat.as_str() {
            "failed" => (Mood::Failed, 1500),
            "finished" if !wincing => (Mood::Finished, 1200),
            _ => return None,
        };
        let until = now_ms + ms;
        self.current = Some((mood, until));
        Some(until)
    }

    pub fn mood(&self, now_ms: i64) -> Option<Mood> {
        self.current.filter(|(_, until)| *until > now_ms).map(|(m, _)| m)
    }
}

pub struct CompanionCanvas {
    pub look: CompanionLook,
    pub mood: Mood,
    pub phase: f32,
    pub poked: bool,
}

impl<M> canvas::Program<M> for CompanionCanvas {
    type State = ();

    fn draw(&self, _state: &Self::State, renderer: &Renderer, theme: &Theme, bounds: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry<Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let h = colors(theme);
        let dark = h.dark;
        let s = bounds.width.min(bounds.height) / 100.0;
        let ox = (bounds.width - 100.0 * s) * 0.5;
        let oy = (bounds.height - 100.0 * s) * 0.5;
        let pt = |x: f32, y: f32| Point::new(ox + (50.0 + x) * s, oy + (55.0 + y) * s);

        let offline = self.mood == Mood::Offline;
        let base_body = if offline { Color::from_rgb(0.52, 0.53, 0.55) } else { project_color(&self.look.body, dark) };
        let body = if offline { alpha(base_body, 0.66) } else { base_body };
        let tint = mix(body, Color::BLACK, 0.20);
        let perk = match self.mood {
            Mood::Attention => 4.0,
            Mood::Working => (self.phase * 3.0).sin() * 2.0 + 1.5,
            _ if self.poked => 4.5,
            _ => 0.0,
        };

        // Ears
        if self.look.ears == "round" {
            for side in [-1.0_f32, 1.0] {
                let c = pt(side * 28.0, -24.0 - perk);
                frame.fill(&Path::circle(c, 11.5 * s), tint);
                frame.fill(&Path::circle(c, 7.0 * s), mix(body, Color::WHITE, 0.36));
            }
        } else if self.look.ears == "pointy" {
            for side in [-1.0_f32, 1.0] {
                let ear = Path::new(|b| {
                    b.move_to(pt(side * 14.0, -27.0));
                    b.quadratic_curve_to(pt(side * 29.0, -48.0 - perk), pt(side * 34.0, -40.0 - perk));
                    b.quadratic_curve_to(pt(side * 39.0, -29.0), pt(side * 36.0, -17.0));
                    b.close();
                });
                frame.fill(&ear, tint);
            }
        }

        // Antenna
        if self.look.antenna {
            let ax = perk * 0.65 - 3.0;
            let ay = -43.0 - perk * 0.2;
            let stem = Path::new(|b| {
                b.move_to(pt(0.0, -32.0));
                b.quadratic_curve_to(pt(3.0, ay + 5.0), pt(ax, ay));
            });
            frame.stroke(&stem, Stroke::default().with_width(3.6 * s).with_color(tint));
            frame.fill(&Path::circle(pt(ax, ay), 5.5 * s), tint);
            let glow = match self.mood {
                Mood::Working | Mood::Attention => 0.75,
                _ => 0.35,
            };
            frame.fill(&Path::circle(pt(ax, ay - 0.6), 3.8 * s), mix(body, Color::WHITE, glow));
        }

        // Body sphere
        let center = pt(0.0, 0.0);
        let radius = 38.5 * s;
        frame.fill(&Path::circle(center, radius), body);
        frame.fill(&Path::circle(pt(-11.0, -14.0), radius * 0.45), alpha(Color::WHITE, if dark { 0.16 } else { 0.28 }));
        frame.stroke(&Path::circle(center, radius), Stroke::default().with_width(1.6 * s).with_color(alpha(Color::WHITE, 0.30)));

        // Gaze direction towards cursor when `look.follow` is on.
        let gaze = if self.look.follow && !offline {
            if let Some(pos) = cursor.position_in(bounds) {
                let dx = (pos.x - center.x) / (bounds.width.max(1.0) * 0.5);
                let dy = (pos.y - center.y) / (bounds.height.max(1.0) * 0.5);
                Vector::new(dx.clamp(-1.0, 1.0), dy.clamp(-1.0, 1.0))
            } else {
                Vector::new(0.0, 0.0)
            }
        } else {
            Vector::new(0.0, 0.0)
        };

        let (ew, eh) = match self.look.eye_shape.as_str() {
            "oval" => (15.0_f32, 20.0_f32),
            "pill" => (13.0, 21.0),
            "sleepy" => (17.0, 13.0),
            _ => (17.0, 17.0),
        };
        let pupil_ratio = match self.look.pupil_size.as_str() {
            "small" => 0.44_f32,
            "large" => 0.68,
            _ => 0.56,
        };
        let ink = Color::from_rgb(0.06, 0.08, 0.15);
        let pupil_color = if self.look.pupil == "ink" { ink } else { project_color(&self.look.pupil, dark) };
        let blink_closed = !offline && self.look.blink != "rare" && (self.phase % 7.0) > 6.75;

        for side in [-1.0_f32, 1.0] {
            let ex = side * 14.0;
            let ey = 1.0;
            if offline {
                let lid = Path::new(|b| {
                    b.move_to(pt(ex - 7.0, ey));
                    b.quadratic_curve_to(pt(ex, ey + 7.0), pt(ex + 7.0, ey));
                });
                frame.stroke(&lid, Stroke::default().with_width(3.0 * s).with_color(alpha(ink, 0.8)));
            } else if self.mood == Mood::Failed {
                let cross = Path::new(|b| {
                    b.move_to(pt(ex - 5.0, ey - 5.0));
                    b.line_to(pt(ex + 5.0, ey + 5.0));
                    b.move_to(pt(ex + 5.0, ey - 5.0));
                    b.line_to(pt(ex - 5.0, ey + 5.0));
                });
                frame.stroke(&cross, Stroke::default().with_width(3.0 * s).with_color(ink));
            } else if self.mood == Mood::Finished || self.poked || blink_closed {
                let smile = Path::new(|b| {
                    b.move_to(pt(ex - 7.0, ey));
                    b.quadratic_curve_to(pt(ex, ey - 10.0), pt(ex + 7.0, ey));
                });
                frame.stroke(&smile, Stroke::default().with_width(3.0 * s).with_color(ink));
            } else {
                let top_left = pt(ex - ew * 0.5, ey - eh * 0.5);
                let eye_path = Path::rounded_rectangle(top_left, Size::new(ew * s, eh * s), (ew.min(eh) * 0.5 * s).into());
                frame.fill(&eye_path, Color::from_rgba(1.0, 1.0, 1.0, 0.98));
                let diam = ew.min(eh) * pupil_ratio;
                let px = ex + gaze.x * (ew - diam) * 0.4;
                let py = ey + gaze.y * (eh - diam).max(0.0) * 0.4;
                frame.fill(&Path::circle(pt(px, py), diam * 0.5 * s), pupil_color);
                frame.fill(&Path::circle(pt(px - diam * 0.17, py - diam * 0.19), diam * 0.14 * s), Color::WHITE);
            }

            if self.look.eyebrows && !offline {
                let level = ey - eh * 0.5 - 5.0 - perk * 0.45;
                let brow_tilt = match self.mood {
                    Mood::Working => 2.0,
                    Mood::Attention => -2.0,
                    _ => 0.0,
                };
                let brow = Path::new(|b| {
                    b.move_to(pt(ex - 4.0, level + brow_tilt * side * 0.5));
                    b.quadratic_curve_to(pt(ex, level - 2.6), pt(ex + 4.0, level - brow_tilt * side * 0.5));
                });
                frame.stroke(&brow, Stroke::default().with_width(2.0 * s).with_color(alpha(ink, 0.72)));
            }
        }

        vec![frame.into_geometry()]
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a,
    }
}

pub fn view<'a, M: 'a>(look: CompanionLook, mood: Mood, size: f32, phase: f32, poked: bool) -> Element<'a, M> {
    Canvas::new(CompanionCanvas { look, mood, phase, poked })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reacts_to_new_beats_on_the_same_server_only() {
        let mut r = Reaction::default();
        let beat = |b: &str, seq| ActivityView { beat: b.into(), beat_seq: seq, ..Default::default() };
        assert_eq!(r.observe("s1", &beat("finished", 4), 0), None, "the beat already there at launch");
        assert_eq!(r.observe("s1", &beat("failed", 5), 1000), Some(2500));
        assert_eq!(r.mood(2000), Some(Mood::Failed));
        assert_eq!(r.observe("s1", &beat("finished", 6), 2000), None, "a wince isn't cut short");
        assert_eq!(r.observe("s1", &beat("finished", 7), 3000), Some(4200));
        assert_eq!(r.mood(4300), None);
        assert_eq!(r.observe("s2", &beat("finished", 1), 5000), None, "switching servers stays quiet");
    }
}
