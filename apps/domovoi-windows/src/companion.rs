//! The companion: a server's small round face, drawn from its look
//! (`CompanionLook`) and what the server is doing. It perks up while agents
//! work, looks alert when one waits for an answer, smiles or winces for a
//! moment when a turn ends, and sleeps while there is no connection.
//!
//! The other clients paint it frame by frame. Here it is a drawing made once
//! for each look and mood, so it holds still between moods.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;

use domovoi_core::companion::CompanionLook;
use domovoi_core::view::ActivityView;
use domovoi_desktop_host::palette;
use windows_reactor::EncodedImage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Offline,
    Attention,
    Working,
    Finished,
    Failed,
    Idle,
}

/// The mood: what the server is doing, or for a moment how a turn just ended (`Reaction`).
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

/// A short reaction when a turn ends: glad for 1.2 s, a wince for 1.5 s (a
/// wince is not cut short by a turn that finishes right after). Only a new
/// beat of the same server counts, so connecting or changing servers stays
/// quiet.
#[derive(Debug, Default)]
pub struct Reaction {
    server: String,
    seq: u64,
    current: Option<(Mood, i64)>,
}

impl Reaction {
    /// Takes the server's latest beat; when it starts a reaction, when that ends (Unix ms).
    pub fn observe(&mut self, server: &str, activity: &ActivityView, now_ms: i64) -> Option<i64> {
        let fresh = self.server == server && self.seq != activity.beat_seq && self.seq != 0;
        self.server = server.to_owned();
        self.seq = activity.beat_seq;
        if !fresh {
            return None;
        }
        let wincing = matches!(self.current, Some((Mood::Failed, until)) if until > now_ms);
        let (mood, lasts) = match activity.beat.as_str() {
            "failed" => (Mood::Failed, 1500),
            "finished" if !wincing => (Mood::Finished, 1200),
            _ => return None,
        };
        self.current = Some((mood, now_ms + lasts));
        Some(now_ms + lasts)
    }

    pub fn mood(&self, now_ms: i64) -> Option<Mood> {
        self.current.filter(|(_, until)| *until > now_ms).map(|(mood, _)| mood)
    }
}

/// `from` moved `share` of the way to `to`, channel by channel.
fn mix(from: u32, to: u32, share: f64) -> u32 {
    let channel = |shift: u32| {
        let (a, b) = (f64::from((from >> shift) & 0xFF), f64::from((to >> shift) & 0xFF));
        ((a + (b - a) * share).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

const INK: u32 = 0x0F1426;

/// The face as an SVG drawing, 100 by 100.
fn svg(look: &CompanionLook, mood: Mood, dark: bool) -> String {
    let offline = mood == Mood::Offline;
    let body = if offline { 0x85878C } else { palette::project_rgb(&look.body, dark) };
    let shade = mix(body, 0x000000, 0.20);
    // How far the ears and the antenna are raised.
    let perk = match mood {
        Mood::Attention => 4.0,
        Mood::Working => 1.5,
        _ => 0.0,
    };
    // The drawing's own origin is the middle of the body.
    let (x, y) = (|dx: f64| 50.0 + dx, |dy: f64| 55.0 + dy);
    let mut out = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">"#);
    // Writing into a `String` cannot fail.
    macro_rules! draw {
        ($($arg:tt)*) => { let _ = write!(out, $($arg)*); };
    }
    for side in [-1.0, 1.0] {
        match look.ears.as_str() {
            "round" => {
                draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="11.5" fill="#{shade:06x}"/>"##, x(side * 28.0), y(-24.0 - perk));
                draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="7" fill="#{:06x}"/>"##, x(side * 28.0), y(-24.0 - perk), mix(body, 0xFFFFFF, 0.36));
            }
            "pointy" => {
                draw!(
                    r##"<path d="M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1} Z" fill="#{shade:06x}"/>"##,
                    x(side * 14.0),
                    y(-27.0),
                    x(side * 29.0),
                    y(-48.0 - perk),
                    x(side * 34.0),
                    y(-40.0 - perk),
                    x(side * 39.0),
                    y(-29.0),
                    x(side * 36.0),
                    y(-17.0)
                );
            }
            _ => {}
        }
    }
    if look.antenna {
        let (tip_x, tip_y) = (perk * 0.65 - 3.0, -43.0 - perk * 0.2);
        let glow = if matches!(mood, Mood::Working | Mood::Attention) { 0.75 } else { 0.35 };
        draw!(
            r##"<path d="M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1}" fill="none" stroke="#{shade:06x}" stroke-width="3.6" stroke-linecap="round"/>"##,
            x(0.0),
            y(-32.0),
            x(3.0),
            y(tip_y + 5.0),
            x(tip_x),
            y(tip_y)
        );
        draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="5.5" fill="#{shade:06x}"/>"##, x(tip_x), y(tip_y));
        draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="3.8" fill="#{:06x}"/>"##, x(tip_x), y(tip_y - 0.6), mix(body, 0xFFFFFF, glow));
    }
    draw!(r##"<circle cx="50" cy="55" r="38.5" fill="#{body:06x}" fill-opacity="{}"/>"##, if offline { 0.66 } else { 1.0 });
    draw!(r##"<circle cx="39" cy="41" r="17.3" fill="#ffffff" fill-opacity="{}"/>"##, if dark { 0.16 } else { 0.28 });
    draw!(r##"<circle cx="50" cy="55" r="38.5" fill="none" stroke="#ffffff" stroke-opacity="0.3" stroke-width="1.6"/>"##);

    let (eye_width, eye_height): (f64, f64) = match look.eye_shape.as_str() {
        "oval" => (15.0, 20.0),
        "pill" => (13.0, 21.0),
        "sleepy" => (17.0, 13.0),
        _ => (17.0, 17.0),
    };
    let pupil_share = match look.pupil_size.as_str() {
        "small" => 0.44,
        "large" => 0.68,
        _ => 0.56,
    };
    let pupil = if look.pupil == "ink" { INK } else { palette::project_rgb(&look.pupil, dark) };
    for side in [-1.0, 1.0] {
        let (eye_x, eye_y) = (side * 14.0, 1.0);
        let line = |d: String, opacity: f64| format!(r##"<path d="{d}" fill="none" stroke="#{INK:06x}" stroke-opacity="{opacity}" stroke-width="3" stroke-linecap="round"/>"##);
        match mood {
            // Shut, as in sleep.
            Mood::Offline => out.push_str(&line(format!("M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1}", x(eye_x - 7.0), y(eye_y), x(eye_x), y(eye_y + 7.0), x(eye_x + 7.0), y(eye_y)), 0.8)),
            Mood::Failed => {
                let cross = format!(
                    "M{:.1} {:.1} L{:.1} {:.1} M{:.1} {:.1} L{:.1} {:.1}",
                    x(eye_x - 5.0),
                    y(eye_y - 5.0),
                    x(eye_x + 5.0),
                    y(eye_y + 5.0),
                    x(eye_x + 5.0),
                    y(eye_y - 5.0),
                    x(eye_x - 5.0),
                    y(eye_y + 5.0)
                );
                out.push_str(&line(cross, 1.0));
            }
            // Shut in a smile.
            Mood::Finished => out.push_str(&line(format!("M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1}", x(eye_x - 7.0), y(eye_y), x(eye_x), y(eye_y - 10.0), x(eye_x + 7.0), y(eye_y)), 1.0)),
            _ => {
                let width = eye_width.min(eye_height) * pupil_share;
                draw!(
                    r##"<rect x="{:.1}" y="{:.1}" width="{eye_width}" height="{eye_height}" rx="{:.1}" fill="#ffffff" fill-opacity="0.98"/>"##,
                    x(eye_x - eye_width / 2.0),
                    y(eye_y - eye_height / 2.0),
                    eye_width.min(eye_height) / 2.0
                );
                draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="{:.2}" fill="#{pupil:06x}"/>"##, x(eye_x), y(eye_y), width / 2.0);
                draw!(r##"<circle cx="{:.1}" cy="{:.1}" r="{:.2}" fill="#ffffff"/>"##, x(eye_x - width * 0.17), y(eye_y - width * 0.19), width * 0.14);
            }
        }
        if look.eyebrows && !offline {
            let level = eye_y - eye_height / 2.0 - 5.0 - perk * 0.45;
            let tilt = match mood {
                Mood::Working => 2.0,
                Mood::Attention => -2.0,
                _ => 0.0,
            };
            draw!(
                r##"<path d="M{:.1} {:.1} Q{:.1} {:.1} {:.1} {:.1}" fill="none" stroke="#{INK:06x}" stroke-opacity="0.72" stroke-width="2" stroke-linecap="round"/>"##,
                x(eye_x - 4.0),
                y(level + tilt * side * 0.5),
                x(eye_x),
                y(level - 2.6),
                x(eye_x + 4.0),
                y(level - tilt * side * 0.5)
            );
        }
    }
    out.push_str("</svg>");
    out
}

thread_local! {
    /// One image per look, mood and theme: the same bytes each render, so the toolkit decodes each once.
    static DRAWN: RefCell<HashMap<String, EncodedImage>> = RefCell::new(HashMap::new());
}

/// The face with `look` in `mood`, for a dark or light window.
pub fn image(look: &CompanionLook, mood: Mood, dark: bool) -> EncodedImage {
    DRAWN.with(|drawn| drawn.borrow_mut().entry(format!("{look:?} {mood:?} {dark}")).or_insert_with(|| EncodedImage::new(svg(look, mood, dark).into_bytes())).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_new_beat_of_the_same_server_is_reacted_to() {
        let mut reaction = Reaction::default();
        let beat = |beat: &str, beat_seq| ActivityView { beat: beat.into(), beat_seq, ..ActivityView::default() };
        assert_eq!(reaction.observe("s1", &beat("finished", 4), 0), None, "the beat that was there at launch");
        assert_eq!(reaction.observe("s1", &beat("failed", 5), 1000), Some(2500));
        assert_eq!(reaction.mood(2000), Some(Mood::Failed));
        assert_eq!(reaction.observe("s1", &beat("finished", 6), 2000), None, "a wince is not cut short");
        assert_eq!(reaction.observe("s1", &beat("finished", 7), 3000), Some(4200));
        assert_eq!(reaction.mood(4300), None);
        assert_eq!(reaction.observe("s2", &beat("finished", 1), 5000), None, "another server stays quiet");
    }

    #[test]
    fn the_mood_is_what_the_server_does_before_how_a_turn_ended() {
        let activity = |working, attention| ActivityView { working, attention, ..ActivityView::default() };
        assert_eq!(mood_for(false, &activity(1, 1), None), Mood::Offline);
        assert_eq!(mood_for(true, &activity(1, 1), Some(Mood::Finished)), Mood::Attention);
        assert_eq!(mood_for(true, &activity(1, 0), Some(Mood::Finished)), Mood::Working);
        assert_eq!(mood_for(true, &activity(0, 0), Some(Mood::Finished)), Mood::Finished);
        assert_eq!(mood_for(true, &activity(0, 0), None), Mood::Idle);
    }

    #[test]
    fn the_face_has_what_its_look_says_and_sleeps_without_a_server() {
        let look = CompanionLook { body: "green".into(), eye_shape: "round".into(), pupil: "ink".into(), ears: "round".into(), antenna: true, eyebrows: true, ..CompanionLook::default() };
        let awake = svg(&look, Mood::Idle, true);
        assert!(awake.contains("#4ade80"), "the body's colour for a dark window");
        assert_eq!(awake.matches("<rect").count(), 2, "two open eyes");
        assert_eq!(awake.matches(r#"r="11.5""#).count(), 2, "two round ears");
        let asleep = svg(&look, Mood::Offline, true);
        assert!(!asleep.contains("<rect") && asleep.contains("#85878c"));
        let bare = svg(&CompanionLook { ears: "none".into(), antenna: false, eyebrows: false, ..look }, Mood::Idle, false);
        assert!(!bare.contains(r#"r="11.5""#) && !bare.contains("stroke-width=\"3.6\""));
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
    }
}
