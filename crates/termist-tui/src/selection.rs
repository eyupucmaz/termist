//! Text picked out of the pane with the mouse: from where the button went down to
//! where it is now, in pane cells, read the way terminals read it (line by line).
use termist_core::{SessionId, Snapshot, cell_flags};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub session: SessionId,
    /// Where the drag started, `(col, row)` inside the pane.
    pub anchor: (u16, u16),
    /// Where the mouse is now.
    pub head: (u16, u16),
    /// Characters put on the clipboard when the button came up.
    pub copied: Option<usize>,
}

impl Selection {
    pub fn new(session: SessionId, at: (u16, u16)) -> Selection {
        Selection {
            session,
            anchor: at,
            head: at,
            copied: None,
        }
    }

    /// The first and the last cell, in reading order.
    fn ends(&self) -> ((u16, u16), (u16, u16)) {
        let (a, h) = (self.anchor, self.head);
        if (a.1, a.0) <= (h.1, h.0) {
            (a, h)
        } else {
            (h, a)
        }
    }

    pub fn contains(&self, col: u16, row: u16) -> bool {
        let (start, end) = self.ends();
        (row, col) >= (start.1, start.0) && (row, col) <= (end.1, end.0)
    }

    /// The selected text: trailing blanks of each line dropped, lines joined with `\n`.
    pub fn text(&self, screen: &Snapshot) -> String {
        let (start, end) = self.ends();
        let mut lines = Vec::new();
        for row in start.1..=end.1 {
            let Some(cells) = screen.lines.get(row as usize) else {
                break;
            };
            let from = if row == start.1 { start.0 as usize } else { 0 };
            let to = if row == end.1 {
                end.0 as usize + 1
            } else {
                cells.len()
            };
            let line: String = cells
                .iter()
                .take(to)
                .skip(from)
                .filter(|c| c.flags & cell_flags::WIDE_SPACER == 0)
                .map(|c| c.ch)
                .collect();
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: &[&str]) -> Snapshot {
        let mut s = Snapshot::blank(12, rows.len() as u16);
        for (r, text) in rows.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                s.lines[r][c].ch = ch;
            }
        }
        s
    }

    fn sel(anchor: (u16, u16), head: (u16, u16)) -> Selection {
        Selection {
            session: SessionId::new(),
            anchor,
            head,
            copied: None,
        }
    }

    #[test]
    fn one_line_runs_between_the_ends_inclusive() {
        let s = screen(&["hello world"]);
        assert_eq!(sel((1, 0), (3, 0)).text(&s), "ell");
        assert_eq!(sel((3, 0), (1, 0)).text(&s), "ell", "dragged backwards");
    }

    #[test]
    fn lines_run_to_the_edge_and_lose_their_trailing_blanks() {
        let s = screen(&["ab  ", "cdef", "gh"]);
        assert_eq!(sel((1, 0), (0, 2)).text(&s), "b\ncdef\ng");
        assert_eq!(sel((0, 2), (1, 0)).text(&s), "b\ncdef\ng", "dragged up");
    }

    #[test]
    fn a_wide_character_is_copied_once() {
        let mut s = screen(&["a"]);
        s.lines[0][1].ch = '界';
        s.lines[0][1].flags = cell_flags::WIDE;
        s.lines[0][2].flags = cell_flags::WIDE_SPACER;
        s.lines[0][3].ch = 'b';
        assert_eq!(sel((0, 0), (3, 0)).text(&s), "a界b");
    }

    #[test]
    fn cells_past_the_screen_are_ignored() {
        let s = screen(&["abc"]);
        assert_eq!(sel((1, 0), (40, 9)).text(&s), "bc");
    }

    #[test]
    fn contains_follows_reading_order() {
        let s = sel((5, 1), (2, 3));
        assert!(!s.contains(4, 1));
        assert!(s.contains(5, 1));
        assert!(s.contains(0, 2), "a middle line is whole");
        assert!(s.contains(9, 2));
        assert!(s.contains(2, 3));
        assert!(!s.contains(3, 3));
        assert!(!s.contains(0, 4));
    }
}
