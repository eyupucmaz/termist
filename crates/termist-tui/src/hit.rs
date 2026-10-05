//! Where the last frame put what the mouse can click: the project tabs, the cards and
//! the lines that count the cards out of sight. Drawing writes it; `App::on_mouse`
//! reads it, so a click lands on what was on screen.
use ratatui::layout::{Position, Rect};
use termist_core::{ProjectId, SessionId};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    /// Each project tab drawn on the header row: its first column and the one after it.
    pub tabs: Vec<(ProjectId, u16, u16)>,
    pub header: Rect,
    pub cards: Vec<(SessionId, Rect)>,
    /// `↑ n more` and `↓ n more`.
    pub above: Option<Rect>,
    pub below: Option<Rect>,
}

impl Hits {
    pub fn tab_at(&self, x: u16, y: u16) -> Option<ProjectId> {
        if !self.header.contains(Position::new(x, y)) {
            return None;
        }
        self.tabs
            .iter()
            .find(|(_, from, to)| (*from..*to).contains(&x))
            .map(|(id, ..)| *id)
    }

    pub fn card_at(&self, x: u16, y: u16) -> Option<SessionId> {
        self.cards
            .iter()
            .find(|(_, r)| r.contains(Position::new(x, y)))
            .map(|(id, _)| *id)
    }

    /// `-1` on the line above the cards, `1` on the one below.
    pub fn more_at(&self, x: u16, y: u16) -> Option<isize> {
        let at = Position::new(x, y);
        if self.above.is_some_and(|r| r.contains(at)) {
            return Some(-1);
        }
        self.below.is_some_and(|r| r.contains(at)).then_some(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_cards_and_more_lines_are_found_where_drawn() {
        let (a, b) = (ProjectId::new(), ProjectId::new());
        let card = SessionId::new();
        let hits = Hits {
            tabs: vec![(a, 10, 16), (b, 17, 24)],
            header: Rect::new(0, 0, 80, 1),
            cards: vec![(card, Rect::new(0, 1, 24, 4))],
            above: None,
            below: Some(Rect::new(0, 5, 24, 1)),
        };
        assert_eq!(hits.tab_at(10, 0), Some(a));
        assert_eq!(hits.tab_at(16, 0), None, "the space between");
        assert_eq!(hits.tab_at(20, 0), Some(b));
        assert_eq!(hits.tab_at(20, 1), None, "only on the header row");
        assert_eq!(hits.card_at(23, 4), Some(card));
        assert_eq!(hits.card_at(24, 4), None);
        assert_eq!(hits.more_at(3, 5), Some(1));
        assert_eq!(hits.more_at(3, 0), None);
    }
}
