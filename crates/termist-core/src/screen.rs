use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub mod cell_flags {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const INVERSE: u16 = 1 << 3;
    pub const DIM: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKEOUT: u16 = 1 << 6;
    /// First half of a double-width character.
    pub const WIDE: u16 = 1 << 7;
    /// Second half of a double-width character: draw nothing here.
    pub const WIDE_SPACER: u16 = 1 << 8;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modes {
    pub alt_screen: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    pub sgr_mouse: bool,
    pub focus_events: bool,
    pub show_cursor: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Vec<Cell>>,
    pub cursor: Cursor,
    pub modes: Modes,
}

/// Rows that changed since the last snapshot a client saw, plus cursor and modes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenUpdate {
    pub cols: u16,
    pub rows: u16,
    pub changed: Vec<(u16, Vec<Cell>)>,
    pub cursor: Cursor,
    pub modes: Modes,
}

impl Snapshot {
    pub fn blank(cols: u16, rows: u16) -> Snapshot {
        Snapshot {
            cols,
            rows,
            lines: vec![vec![Cell::default(); cols as usize]; rows as usize],
            cursor: Cursor::default(),
            modes: Modes {
                show_cursor: true,
                ..Modes::default()
            },
        }
    }

    pub fn apply(&mut self, u: &ScreenUpdate) {
        if u.cols != self.cols || u.rows != self.rows {
            *self = Snapshot::blank(u.cols, u.rows);
        }
        for (row, cells) in &u.changed {
            if let Some(line) = self.lines.get_mut(*row as usize) {
                *line = cells.clone();
            }
        }
        self.cursor = u.cursor;
        self.modes = u.modes;
    }

    pub fn line_text(&self, row: usize) -> String {
        self.lines
            .get(row)
            .map(|l| {
                l.iter()
                    .filter(|c| c.flags & cell_flags::WIDE_SPACER == 0)
                    .map(|c| c.ch)
                    .collect::<String>()
            })
            .unwrap_or_default()
            .trim_end()
            .to_string()
    }
}

/// What a client that last saw `old` needs to reach `new`. `None` when nothing changed.
pub fn diff(old: Option<&Snapshot>, new: &Snapshot) -> Option<ScreenUpdate> {
    let same_shape = old.is_some_and(|o| o.cols == new.cols && o.rows == new.rows);
    let changed: Vec<(u16, Vec<Cell>)> = new
        .lines
        .iter()
        .enumerate()
        .filter(|(r, line)| !same_shape || old.unwrap().lines.get(*r) != Some(*line))
        .map(|(r, line)| (r as u16, line.clone()))
        .collect();
    if same_shape {
        let o = old.unwrap();
        if changed.is_empty() && o.cursor == new.cursor && o.modes == new.modes {
            return None;
        }
    }
    Some(ScreenUpdate {
        cols: new.cols,
        rows: new.rows,
        changed,
        cursor: new.cursor,
        modes: new.modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_with(text: &[&str]) -> Snapshot {
        let mut s = Snapshot::blank(5, text.len() as u16);
        for (r, line) in text.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                s.lines[r][c].ch = ch;
            }
        }
        s
    }

    #[test]
    fn first_diff_sends_every_row() {
        let new = snap_with(&["ab", "cd"]);
        let u = diff(None, &new).unwrap();
        assert_eq!(u.changed.len(), 2);
    }

    #[test]
    fn only_changed_rows_are_sent_and_apply_reconstructs() {
        let old = snap_with(&["ab", "cd", "ef"]);
        let new = snap_with(&["ab", "cX", "ef"]);
        let u = diff(Some(&old), &new).unwrap();
        assert_eq!(
            u.changed.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            vec![1]
        );
        let mut copy = old.clone();
        copy.apply(&u);
        assert_eq!(copy, new);
    }

    #[test]
    fn no_change_means_no_update_but_cursor_moves_count() {
        let old = snap_with(&["ab"]);
        assert!(diff(Some(&old), &old.clone()).is_none());
        let mut moved = old.clone();
        moved.cursor.col = 2;
        let u = diff(Some(&old), &moved).unwrap();
        assert!(u.changed.is_empty());
        assert_eq!(u.cursor.col, 2);
    }

    #[test]
    fn a_resize_resends_everything() {
        let old = snap_with(&["ab"]);
        let new = Snapshot::blank(7, 3);
        let u = diff(Some(&old), &new).unwrap();
        assert_eq!(u.changed.len(), 3);
        let mut copy = old.clone();
        copy.apply(&u);
        assert_eq!(copy, new);
    }

    #[test]
    fn line_text_trims_trailing_blanks() {
        assert_eq!(snap_with(&["hi"]).line_text(0), "hi");
    }
}
