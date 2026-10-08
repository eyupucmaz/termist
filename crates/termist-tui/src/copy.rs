//! Copy mode: a cursor over a session's history (`C-a [`), moved as in vi, a selection
//! by characters (`v`) or lines (`V`) copied with `y`, and `/` `?` searches. Places are
//! counted from the top of the history, so they stay put while the view moves; the
//! text and the searches come from the daemon, which has all of the history.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termist_core::{Pos, Scroll, SessionId, Snapshot};

/// What a key in copy mode asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyOut {
    /// Move the view (one `Scroll` after another).
    Scroll(Vec<Scroll>),
    Search {
        query: String,
        from: Pos,
        backward: bool,
    },
    Yank {
        from: Pos,
        to: Pos,
        lines: bool,
    },
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    pub session: SessionId,
    pub cursor: Pos,
    /// Where a selection began; it runs to the cursor.
    pub anchor: Option<Pos>,
    /// The selection is of whole lines (`V`).
    pub lines: bool,
    /// The search being typed, and whether it goes up (`?`).
    pub typing: Option<(String, bool)>,
    /// The last search, for `n` and `N`.
    pub last: Option<(String, bool)>,
    /// Which of how many the last found place was.
    pub found: Option<(u32, u32)>,
    /// Said in the pane's title for a moment: `no match`.
    pub note: Option<&'static str>,
}

/// The first line of the history the view shows.
pub fn top(screen: &Snapshot) -> u32 {
    screen.scroll.history.saturating_sub(screen.scroll.offset)
}

/// The last line there is: the live screen's last.
fn last_line(screen: &Snapshot) -> u32 {
    screen.scroll.history + screen.rows.saturating_sub(1) as u32
}

/// The text of history line `line`, when the view shows it.
fn line_text(screen: &Snapshot, line: u32) -> Option<Vec<char>> {
    let row = line.checked_sub(top(screen))?;
    (row < screen.rows as u32).then(|| screen.line_text(row as usize).chars().collect())
}

/// What a character is, for words: blank, or part of one (a WORD, as vi's `W`).
fn blank(c: char) -> bool {
    c.is_whitespace() || c == '\0'
}

impl Copy {
    /// Copy mode on `session`, its cursor on the live screen's last line.
    pub fn new(session: SessionId, screen: &Snapshot) -> Copy {
        Copy {
            session,
            cursor: Pos {
                line: last_line(screen),
                col: 0,
            },
            anchor: None,
            lines: false,
            typing: None,
            last: None,
            found: None,
            note: None,
        }
    }

    /// Whether the cell at `line`, `col` is selected.
    pub fn selected(&self, line: u32, col: u16) -> bool {
        let Some(anchor) = self.anchor else {
            return false;
        };
        let (a, b) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        if self.lines {
            return (a.line..=b.line).contains(&line);
        }
        let here = Pos { line, col };
        a <= here && here <= b
    }

    /// The view put where the cursor is on it: a place, so keys that come before the
    /// screen moved do not move it twice.
    fn follow(&self, screen: &Snapshot) -> Vec<CopyOut> {
        let (top, rows) = (top(screen), screen.rows.max(1) as u32);
        let want = if self.cursor.line < top {
            self.cursor.line
        } else if self.cursor.line >= top + rows {
            self.cursor.line + 1 - rows
        } else {
            return vec![];
        };
        let mut scroll = vec![Scroll::Top];
        if want > 0 {
            scroll.push(Scroll::Lines(-(want as i32)));
        }
        vec![CopyOut::Scroll(scroll)]
    }

    /// The view and the cursor `delta` lines on together, as vi's page keys.
    pub fn page(&mut self, delta: i64, screen: &Snapshot) -> Vec<CopyOut> {
        let rows = screen.rows.max(1) as i64;
        let last_top = (last_line(screen) as i64 + 1 - rows).max(0);
        let from = top(screen) as i64;
        let to = (from + delta).clamp(0, last_top);
        let line = (self.cursor.line as i64 + delta).clamp(to, to + rows - 1);
        self.cursor.line = line.clamp(0, last_line(screen) as i64) as u32;
        if to == from {
            return vec![];
        }
        let mut scroll = vec![Scroll::Top];
        if to > 0 {
            scroll.push(Scroll::Lines(-(to as i32)));
        }
        vec![CopyOut::Scroll(scroll)]
    }

    /// The cursor to `line` (held in the history), then the view after it.
    fn go(&mut self, line: i64, screen: &Snapshot) -> Vec<CopyOut> {
        self.cursor.line = line.clamp(0, last_line(screen) as i64) as u32;
        self.follow(screen)
    }

    /// A key; a page is the screen's height.
    pub fn key(&mut self, key: KeyEvent, screen: &Snapshot) -> Vec<CopyOut> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some((query, backward)) = &mut self.typing {
            match key.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    let (query, backward) = (query.clone(), *backward);
                    self.typing = None;
                    if query.is_empty() {
                        return vec![];
                    }
                    self.last = Some((query.clone(), backward));
                    return vec![CopyOut::Search {
                        query,
                        from: self.cursor,
                        backward,
                    }];
                }
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c) if !ctrl => query.push(c),
                _ => {}
            }
            return vec![];
        }
        self.note = None;
        let line = self.cursor.line as i64;
        let page = screen.rows.max(1) as i64;
        let cols = screen.cols.max(1);
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => {
                self.cursor.col = self.cursor.col.saturating_sub(1)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.cursor.col = (self.cursor.col + 1).min(cols - 1)
            }
            KeyCode::Char('k') | KeyCode::Up if !ctrl => return self.go(line - 1, screen),
            KeyCode::Char('j') | KeyCode::Down if !ctrl => return self.go(line + 1, screen),
            KeyCode::Char('u') if ctrl => return self.page(-(page / 2).max(1), screen),
            KeyCode::Char('d') if ctrl => return self.page((page / 2).max(1), screen),
            KeyCode::Char('b') if ctrl => return self.page(-page, screen),
            KeyCode::Char('f') if ctrl => return self.page(page, screen),
            KeyCode::PageUp => return self.page(-page, screen),
            KeyCode::PageDown => return self.page(page, screen),
            KeyCode::Char('g') | KeyCode::Home => {
                self.cursor.col = 0;
                return self.go(0, screen);
            }
            KeyCode::Char('G') | KeyCode::End => return self.go(i64::MAX / 2, screen),
            KeyCode::Char('0') => self.cursor.col = 0,
            KeyCode::Char('$') => {
                if let Some(text) = line_text(screen, self.cursor.line) {
                    self.cursor.col = text.iter().rposition(|c| !blank(*c)).unwrap_or(0) as u16;
                }
            }
            KeyCode::Char(c @ ('w' | 'b' | 'e')) if !ctrl => {
                if let Some(text) = line_text(screen, self.cursor.line) {
                    self.cursor.col = word_move(&text, self.cursor.col as usize, c) as u16;
                }
            }
            KeyCode::Char('v') if !ctrl => self.select(false),
            KeyCode::Char('V') => self.select(true),
            KeyCode::Char('y') => {
                let Some(from) = self.anchor else {
                    return vec![];
                };
                return vec![
                    CopyOut::Yank {
                        from,
                        to: self.cursor,
                        lines: self.lines,
                    },
                    CopyOut::Exit,
                ];
            }
            KeyCode::Char(c @ ('/' | '?')) => self.typing = Some((String::new(), c == '?')),
            KeyCode::Char(c @ ('n' | 'N')) => {
                let Some((query, backward)) = self.last.clone() else {
                    return vec![];
                };
                return vec![CopyOut::Search {
                    query,
                    from: self.cursor,
                    backward: backward == (c == 'n'),
                }];
            }
            KeyCode::Esc if self.anchor.is_some() => self.anchor = None,
            KeyCode::Esc | KeyCode::Char('q') => return vec![CopyOut::Exit],
            KeyCode::Char('c') if ctrl => return vec![CopyOut::Exit],
            _ => {}
        }
        vec![]
    }

    /// `v` or `V`: a selection from the cursor, or none again.
    fn select(&mut self, lines: bool) {
        if self.anchor.is_some() && self.lines == lines {
            self.anchor = None;
        } else {
            self.anchor.get_or_insert(self.cursor);
            self.lines = lines;
        }
    }

    /// The daemon found the search at `at` (or nowhere): the cursor goes there.
    pub fn found_at(
        &mut self,
        at: Option<(Pos, Pos)>,
        index: u32,
        total: u32,
        screen: &Snapshot,
    ) -> Vec<CopyOut> {
        let Some((start, _)) = at else {
            self.note = Some("no match");
            self.found = None;
            return vec![];
        };
        self.cursor = start;
        self.found = Some((index, total));
        self.follow(screen)
    }
}

/// Where `w` (next word's start), `b` (this or the last word's start) or `e` (this or
/// the next word's end) goes on a line from `col`.
fn word_move(text: &[char], col: usize, how: char) -> usize {
    let n = text.len();
    let word = |i: usize| i < n && !blank(text[i]);
    let mut i = col.min(n.saturating_sub(1));
    match how {
        'w' => {
            while word(i) {
                i += 1;
            }
            while i < n && !word(i) {
                i += 1;
            }
            if i >= n { col } else { i }
        }
        'e' => {
            i += 1;
            while i < n && !word(i) {
                i += 1;
            }
            while word(i + 1) {
                i += 1;
            }
            if i >= n { col } else { i }
        }
        _ => {
            i = i.saturating_sub(1);
            while i > 0 && !word(i) {
                i -= 1;
            }
            while i > 0 && word(i - 1) {
                i -= 1;
            }
            i
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};
    use termist_core::{Cell, ScrollPos};

    /// A 20×5 screen showing history lines from `top` (of 10 history lines), each line's
    /// text from `text`.
    fn screen(offset: u32, text: impl Fn(u32) -> String) -> Snapshot {
        let mut s = Snapshot::blank(20, 5);
        s.scroll = ScrollPos {
            offset,
            history: 10,
        };
        let first = 10 - offset;
        for r in 0..5u32 {
            for (c, ch) in text(first + r).chars().enumerate() {
                s.lines[r as usize][c] = Cell {
                    ch,
                    ..Cell::default()
                };
            }
        }
        s
    }

    fn plain(_: u32) -> String {
        "one two  three".into()
    }

    fn at(line: u32, col: u16) -> Pos {
        Pos { line, col }
    }

    fn press(c: &mut Copy, s: &Snapshot, code: K) -> Vec<CopyOut> {
        c.key(KeyEvent::new(code, M::NONE), s)
    }

    #[test]
    fn the_cursor_starts_on_the_live_screen_s_last_line_and_moves_as_in_vi() {
        let s = screen(0, plain);
        let mut c = Copy::new(SessionId::new(), &s);
        assert_eq!(c.cursor, at(14, 0), "10 history lines, 5 on screen");
        press(&mut c, &s, K::Char('w'));
        assert_eq!(c.cursor, at(14, 4));
        press(&mut c, &s, K::Char('w'));
        assert_eq!(c.cursor, at(14, 9), "past the two spaces");
        press(&mut c, &s, K::Char('e'));
        assert_eq!(c.cursor, at(14, 13));
        press(&mut c, &s, K::Char('b'));
        assert_eq!(c.cursor, at(14, 9));
        press(&mut c, &s, K::Char('$'));
        assert_eq!(
            c.cursor,
            at(14, 13),
            "the last letter, not the blanks after"
        );
        press(&mut c, &s, K::Char('0'));
        assert_eq!(c.cursor, at(14, 0));
        press(&mut c, &s, K::Char('l'));
        press(&mut c, &s, K::Char('k'));
        assert_eq!(c.cursor, at(13, 1));
        assert!(
            press(&mut c, &s, K::Char('j')).is_empty(),
            "on screen: nothing moves"
        );
        assert!(
            press(&mut c, &s, K::Char('j')).is_empty(),
            "the last line stops it"
        );
        assert_eq!(c.cursor, at(14, 1));
    }

    #[test]
    fn leaving_the_screen_moves_the_view_to_a_place_not_by_a_step() {
        let s = screen(0, plain);
        let mut c = Copy::new(SessionId::new(), &s);
        for _ in 0..4 {
            press(&mut c, &s, K::Char('k'));
        }
        assert_eq!(c.cursor.line, 10, "the screen's first line");
        // The screen has not moved yet when the next keys come: each asks for a place.
        assert_eq!(
            press(&mut c, &s, K::Char('k')),
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-9)])]
        );
        assert_eq!(
            press(&mut c, &s, K::Char('k')),
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-8)])]
        );
        assert_eq!(
            press(&mut c, &s, K::Char('g')),
            [CopyOut::Scroll(vec![Scroll::Top])]
        );
        assert_eq!(c.cursor, at(0, 0));
        let back = screen(10, plain);
        assert_eq!(
            press(&mut c, &back, K::Char('G')),
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-10)])]
        );
        assert_eq!(c.cursor.line, 14);
    }

    #[test]
    fn page_keys_move_the_view_and_the_cursor_together() {
        let s = screen(0, plain);
        let mut c = Copy::new(SessionId::new(), &s);
        assert_eq!(
            press(&mut c, &s, K::PageUp),
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-5)])]
        );
        assert_eq!(c.cursor.line, 9);
        let back = screen(5, plain);
        assert_eq!(
            c.key(KeyEvent::new(K::Char('d'), M::CONTROL), &back),
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-7)])]
        );
        assert_eq!(c.cursor.line, 11);
        let top = screen(10, plain);
        let mut c = Copy::new(SessionId::new(), &top);
        c.cursor.line = 2;
        assert_eq!(press(&mut c, &top, K::PageUp), [], "the top stops it");
        assert_eq!(c.cursor.line, 0);
    }

    #[test]
    fn v_selects_characters_and_capital_v_lines_and_y_copies_them() {
        let s = screen(0, plain);
        let mut c = Copy::new(SessionId::new(), &s);
        press(&mut c, &s, K::Char('w'));
        press(&mut c, &s, K::Char('v'));
        press(&mut c, &s, K::Char('k'));
        assert!(c.selected(13, 10) && c.selected(14, 4) && !c.selected(14, 5));
        assert!(!c.selected(13, 3), "before the start");
        assert_eq!(
            press(&mut c, &s, K::Char('y')),
            [
                CopyOut::Yank {
                    from: at(14, 4),
                    to: at(13, 4),
                    lines: false
                },
                CopyOut::Exit
            ]
        );
        let mut c = Copy::new(SessionId::new(), &s);
        press(&mut c, &s, K::Char('V'));
        press(&mut c, &s, K::Char('k'));
        assert!(c.selected(13, 0) && c.selected(14, 19));
        press(&mut c, &s, K::Char('V'));
        assert_eq!(c.anchor, None, "V again lets it go");
        assert_eq!(
            press(&mut c, &s, K::Char('y')),
            [],
            "nothing chosen: nothing to copy"
        );
        assert_eq!(press(&mut c, &s, K::Char('q')), [CopyOut::Exit]);
        press(&mut c, &s, K::Char('v'));
        assert_eq!(
            press(&mut c, &s, K::Esc),
            [],
            "Esc lets the selection go first"
        );
        assert_eq!(press(&mut c, &s, K::Esc), [CopyOut::Exit]);
    }

    #[test]
    fn a_search_is_typed_then_asked_and_n_asks_again_either_way() {
        let s = screen(0, plain);
        let mut c = Copy::new(SessionId::new(), &s);
        press(&mut c, &s, K::Char('?'));
        for ch in "err".chars() {
            press(&mut c, &s, K::Char(ch));
        }
        assert_eq!(c.typing, Some(("err".into(), true)));
        assert_eq!(
            press(&mut c, &s, K::Enter),
            [CopyOut::Search {
                query: "err".into(),
                from: at(14, 0),
                backward: true
            }]
        );
        // Found far up: the cursor goes there, the view after it.
        let outs = c.found_at(Some((at(3, 2), at(3, 4))), 2, 5, &s);
        assert_eq!(c.cursor, at(3, 2));
        assert_eq!(c.found, Some((2, 5)));
        assert_eq!(
            outs,
            [CopyOut::Scroll(vec![Scroll::Top, Scroll::Lines(-3)])]
        );
        assert!(matches!(
            &press(&mut c, &s, K::Char('n'))[..],
            [CopyOut::Search { backward: true, from, .. }] if *from == at(3, 2)
        ));
        assert!(matches!(
            &press(&mut c, &s, K::Char('N'))[..],
            [CopyOut::Search {
                backward: false,
                ..
            }]
        ));
        c.found_at(None, 0, 0, &s);
        assert_eq!((c.note, c.cursor), (Some("no match"), at(3, 2)), "it stays");
        press(&mut c, &s, K::Char('/'));
        assert_eq!(press(&mut c, &s, K::Esc), [], "the search is let go");
        assert_eq!(c.typing, None);
        let _ = KeyModifiers::NONE;
    }
}
