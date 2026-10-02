//! Short notes in the top right corner: a copy, an agent that waits or is done.
//! They go by themselves; at most three show, the newest on top.
use ratatui::layout::Rect;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use termist_core::{AgentStatus, SessionId};

pub const COPIED_FOR: Duration = Duration::from_secs(2);
pub const AGENT_FOR: Duration = Duration::from_secs(6);
/// The widest a toast gets, borders included; longer text ends in `…`.
pub const MAX_WIDTH: u16 = 48;
const MAX_SHOWN: usize = 3;
const HEIGHT: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Copied,
    /// Clicking it goes to the card.
    Agent {
        session: SessionId,
        status: AgentStatus,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Instant,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toasts {
    /// Newest first.
    items: VecDeque<Toast>,
}

impl Toasts {
    pub fn push(&mut self, toast: Toast) {
        self.items.push_front(toast);
        self.items.truncate(MAX_SHOWN);
    }

    pub fn expire(&mut self, now: Instant) {
        self.items.retain(|t| t.until > now);
    }

    pub fn next_expiry(&self) -> Option<Instant> {
        self.items.iter().map(|t| t.until).min()
    }

    pub fn items(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn remove(&mut self, index: usize) -> Option<Toast> {
        self.items.remove(index)
    }

    /// Where each toast goes on `screen`, in `items` order: under the header, against
    /// the right edge with a column to spare, one under the other. A toast that does
    /// not fit gets no rect, and neither does any below it.
    pub fn rects(&self, screen: Rect) -> Vec<Rect> {
        let mut out = Vec::new();
        let mut y = screen.y + 1;
        for toast in &self.items {
            let width = fit(&toast.text).chars().count() as u16 + 4;
            if width + 1 > screen.width || y + HEIGHT > screen.bottom() {
                break;
            }
            out.push(Rect::new(screen.right() - 1 - width, y, width, HEIGHT));
            y += HEIGHT;
        }
        out
    }

    pub fn hit(&self, screen: Rect, col: u16, row: u16) -> Option<usize> {
        self.rects(screen)
            .iter()
            .position(|r| r.contains(ratatui::layout::Position::new(col, row)))
    }
}

/// The text cut to fit `MAX_WIDTH` with its border and padding: `…` at the end.
pub fn fit(text: &str) -> String {
    let room = (MAX_WIDTH - 4) as usize;
    if text.chars().count() <= room {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(room - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copied(text: &str, until: Instant) -> Toast {
        Toast {
            text: text.into(),
            kind: ToastKind::Copied,
            until,
        }
    }

    #[test]
    fn at_most_three_newest_first() {
        let now = Instant::now();
        let mut t = Toasts::default();
        for i in 0..5 {
            t.push(copied(&format!("t{i}"), now + COPIED_FOR));
        }
        let texts: Vec<&str> = t.items().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["t4", "t3", "t2"]);
    }

    #[test]
    fn a_toast_goes_when_its_time_is_up() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(copied("short", now + Duration::from_secs(2)));
        t.push(copied("long", now + Duration::from_secs(6)));
        assert_eq!(t.next_expiry(), Some(now + Duration::from_secs(2)));
        t.expire(now + Duration::from_secs(2));
        let texts: Vec<&str> = t.items().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["long"]);
        assert_eq!(t.next_expiry(), Some(now + Duration::from_secs(6)));
        t.expire(now + Duration::from_secs(7));
        assert!(t.is_empty());
        assert_eq!(t.next_expiry(), None);
    }

    #[test]
    fn toasts_stack_under_the_header_against_the_right_edge() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(copied("one", now));
        t.push(copied("a longer one", now));
        let screen = Rect::new(0, 0, 100, 40);
        let r = t.rects(screen);
        // "a longer one" is 12 wide: border, space, text, space, border = 16.
        assert_eq!(r[0], Rect::new(100 - 1 - 16, 1, 16, 3));
        assert_eq!(r[1], Rect::new(100 - 1 - 7, 4, 7, 3));
    }

    #[test]
    fn long_text_is_cut_to_the_widest_toast() {
        let long = "x".repeat(100);
        let cut = fit(&long);
        assert_eq!(cut.chars().count(), (MAX_WIDTH - 4) as usize);
        assert!(cut.ends_with('…'));
        assert_eq!(fit("short"), "short");
    }

    #[test]
    fn a_narrow_screen_draws_no_toast() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(copied("copied 13 characters", now));
        assert!(t.rects(Rect::new(0, 0, 20, 40)).is_empty(), "too narrow");
        assert!(t.rects(Rect::new(0, 0, 100, 3)).is_empty(), "too short");
    }

    #[test]
    fn a_click_finds_the_toast_under_it() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(copied("one", now));
        t.push(copied("two", now));
        let screen = Rect::new(0, 0, 100, 40);
        let r = t.rects(screen);
        assert_eq!(t.hit(screen, r[0].x, r[0].y), Some(0));
        assert_eq!(t.hit(screen, r[1].x + 1, r[1].y + 2), Some(1));
        assert_eq!(t.hit(screen, 0, 0), None);
    }
}
