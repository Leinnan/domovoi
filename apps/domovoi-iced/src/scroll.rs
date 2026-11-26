//! The transcript's scrolling: it follows the conversation while you are at
//! the bottom, holds still while you read further up (with a "new messages"
//! nudge), remembers where you were in each thread, and can reveal one row
//! (the active find match, the turn's live step).
//!
//! The transcript scrollable is anchored to its bottom, so its offset is the
//! distance from the end: at 0 it keeps up with streamed output by itself.
//! Iced also reports a viewport when only the content's size changed; those
//! reports are told apart from the user's scrolling by the content height.

use std::collections::HashMap;

use domovoi_core::view::ThreadPageView;
use iced::Task;
use iced::advanced::widget::operation::scrollable::{AbsoluteOffset, scroll_to};
use iced::advanced::widget::operation::{Operation, Outcome, Scrollable};
use iced::advanced::widget::{Id, operate};
use iced::widget::scrollable::Viewport;
use iced::{Rectangle, Vector};

/// Within this distance of the bottom counts as "at the bottom".
const FOLLOW_SLACK: f32 = 24.0;
/// Room left above a revealed row.
const REVEAL_MARGIN: f32 = 48.0;

pub fn transcript_id() -> Id {
    Id::new("transcript")
}

/// The id of a transcript row's container (see `views::thread`).
pub fn item_id(item_id: &str) -> Id {
    Id::from(format!("item:{item_id}"))
}

/// What changed in the open thread, cheaply: new rows, a row growing, a card coming or going.
#[derive(Debug, Clone, PartialEq, Default)]
struct Signature {
    rows: usize,
    first_id: String,
    last_id: String,
    last_len: usize,
    cards: usize,
}

impl Signature {
    fn of(t: &ThreadPageView) -> Self {
        let last = t.items.last();
        Self {
            rows: t.items.len(),
            first_id: t.items.first().map(|item| item.id.clone()).unwrap_or_default(),
            last_id: last.map(|i| i.id.clone()).unwrap_or_default(),
            last_len: last.map_or(0, |i| i.text.len() + i.blocks.len() + i.output.as_ref().map_or(0, |o| o.len())),
            cards: t.pending_requests.len() + t.queued.len() + t.pending_sends.len() + t.background_tasks.len() + usize::from(t.working),
        }
    }
}

#[derive(Debug, Default)]
pub struct Transcript {
    thread_id: String,
    signature: Signature,
    /// At the bottom: new output stays in view.
    pub following: bool,
    /// Rows arrived while scrolled up.
    pub unseen: bool,
    /// The last viewport Iced reported (`None` right after a thread switch).
    last: Option<(f32, f32, f32)>,
    /// Distance from the bottom per thread, for threads left scrolled up.
    memory: HashMap<String, f32>,
    find_item: Option<String>,
    history_prepended: bool,
}

impl Transcript {
    pub fn new() -> Self {
        Self { following: true, ..Self::default() }
    }

    /// After the core rendered: restore the position on a thread switch,
    /// note unseen rows, and reveal a new find match.
    pub fn after_render<M: Send + 'static>(&mut self, thread: Option<&ThreadPageView>) -> Task<M> {
        let Some(t) = thread else {
            self.thread_id.clear();
            return Task::none();
        };
        let mut task = Task::none();
        let sig = Signature::of(t);
        if t.id != self.thread_id {
            self.thread_id = t.id.clone();
            self.last = None;
            self.unseen = false;
            self.find_item = None;
            self.history_prepended = false;
            let from_end = self.memory.get(&t.id).copied().unwrap_or(0.0);
            self.following = from_end <= FOLLOW_SLACK;
            task = scroll_to_from_end(from_end);
        } else if sig != self.signature && !self.following && sig.rows > self.signature.rows {
            self.history_prepended = sig.first_id != self.signature.first_id && sig.last_id == self.signature.last_id;
            self.unseen |= !self.history_prepended;
        }
        self.signature = sig;

        let find = t.find.as_ref().and_then(|f| f.active_item_id.clone());
        if find != self.find_item {
            self.find_item = find.clone();
            if let Some(id) = find {
                return Task::batch([task, self.reveal(&id)]);
            }
        }
        task
    }

    /// Iced reported the viewport: the user scrolled, or the content changed size.
    pub fn scrolled<M: Send + 'static>(&mut self, v: Viewport) -> Task<M> {
        let from_end = v.absolute_offset().y.max(0.0);
        let (content_h, bounds_h) = (v.content_bounds().height, v.bounds().height);
        let previous = self.last.replace((from_end, content_h, bounds_h));
        let mut task = Task::none();
        match previous {
            // Only the content grew or shrank: keep what the reader sees where it is.
            Some((prev_from_end, prev_h, prev_bounds)) if (content_h - prev_h).abs() > 0.5 && (bounds_h - prev_bounds).abs() < 0.5 => {
                if !self.following {
                    let top = prev_h - prev_bounds - prev_from_end;
                    let pinned = if std::mem::take(&mut self.history_prepended) {
                        prev_from_end
                    } else {
                        (content_h - bounds_h - top).max(0.0)
                    };
                    self.last = Some((pinned, content_h, bounds_h));
                    task = scroll_to_from_end(pinned);
                    self.remember(pinned);
                }
            }
            // The user scrolled (or the window resized).
            _ => {
                self.following = from_end <= FOLLOW_SLACK;
                if self.following {
                    self.unseen = false;
                }
                self.remember(from_end);
            }
        }
        task
    }

    fn remember(&mut self, from_end: f32) {
        if self.thread_id.is_empty() {
            return;
        }
        if from_end <= FOLLOW_SLACK {
            self.memory.remove(&self.thread_id);
        } else {
            self.memory.insert(self.thread_id.clone(), from_end);
        }
    }

    /// Back to the newest output (the "new messages" button, sending).
    pub fn jump_to_latest<M: Send + 'static>(&mut self) -> Task<M> {
        self.following = true;
        self.unseen = false;
        self.memory.remove(&self.thread_id);
        scroll_to_from_end(0.0)
    }

    /// Scrolls the row `item_id` to near the top of the transcript.
    pub fn reveal<M: Send + 'static>(&mut self, item_id: &str) -> Task<M> {
        self.following = false;
        operate(Reveal { target: self::item_id(item_id), scroll: transcript_id(), viewport: None, row: None }).discard()
    }
}

fn scroll_to_from_end<M: Send + 'static>(from_end: f32) -> Task<M> {
    iced::widget::operation::scroll_to(transcript_id(), AbsoluteOffset { x: None, y: Some(from_end) })
}

/// Finds the transcript's viewport and a row's place in it, then scrolls
/// there (the row's bounds are its unscrolled layout position).
struct Reveal {
    target: Id,
    scroll: Id,
    viewport: Option<(Rectangle, Rectangle)>,
    row: Option<Rectangle>,
}

impl Operation<()> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if self.row.is_none() && id == Some(&self.target) {
            self.row = Some(bounds);
        }
    }

    fn scrollable(&mut self, id: Option<&Id>, bounds: Rectangle, content_bounds: Rectangle, _translation: Vector, _state: &mut dyn Scrollable) {
        if id == Some(&self.scroll) {
            self.viewport = Some((bounds, content_bounds));
        }
    }

    fn finish(&self) -> Outcome<()> {
        let (Some((bounds, content)), Some(row)) = (self.viewport, self.row) else { return Outcome::None };
        Outcome::Chain(Box::new(scroll_to(self.scroll.clone(), AbsoluteOffset { x: None, y: Some(from_end_for(bounds, content, row)) })))
    }
}

/// The bottom-anchored offset that puts `row` just below the top of the view.
fn from_end_for(bounds: Rectangle, content: Rectangle, row: Rectangle) -> f32 {
    let top = (row.y - content.y - REVEAL_MARGIN).max(0.0);
    (content.height - bounds.height - top).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_offset_is_measured_from_the_bottom() {
        let bounds = Rectangle { x: 0.0, y: 100.0, width: 800.0, height: 600.0 };
        let content = Rectangle { x: 0.0, y: 100.0, width: 800.0, height: 3000.0 };
        let row = Rectangle { x: 0.0, y: 1100.0, width: 800.0, height: 40.0 };
        // The row is 1000 px down the content: 48 px above it at the view's top.
        assert_eq!(from_end_for(bounds, content, row), 3000.0 - 600.0 - 952.0);
        // A row near the end can't go higher than the bottom allows.
        assert_eq!(from_end_for(bounds, content, Rectangle { y: 3050.0, ..row }), 0.0);
    }

    #[test]
    fn signature_notices_new_rows_only_while_reading_above() {
        let mut t = Transcript::new();
        let mut page = ThreadPageView { id: "a".into(), ..ThreadPageView::default() };
        let _: Task<()> = t.after_render(Some(&page));
        assert!(t.following && !t.unseen);
        t.following = false;
        page.items.push(domovoi_core::view::ItemView { id: "1".into(), ..Default::default() });
        let _: Task<()> = t.after_render(Some(&page));
        assert!(t.unseen);
        let _: Task<()> = t.jump_to_latest();
        assert!(t.following && !t.unseen);
        // Another thread starts at its bottom and forgets the nudge.
        t.following = false;
        t.unseen = true;
        let _: Task<()> = t.after_render(Some(&ThreadPageView { id: "b".into(), ..ThreadPageView::default() }));
        assert!(t.following && !t.unseen);
    }
}
