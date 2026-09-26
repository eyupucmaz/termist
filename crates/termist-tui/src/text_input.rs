//! The line editor behind every text box (quick prompt, follow-up, rename, model name).
//! It knows nothing about the screen: it turns keys into text and a cursor.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key did to the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The key edited the text or moved the cursor.
    Handled,
    /// Plain Enter: the owner decides what submitting means.
    Submit,
    /// Not an editing key (Esc, Tab, Ctrl+O, …): the owner may use it.
    Ignored,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    /// Shift+Enter, Alt+Enter and Ctrl+J insert a newline only in a multi-line input.
    multiline: bool,
    /// Earlier entries, newest first; `↑` in an empty input walks them.
    history: Vec<String>,
    /// Which history entry the text shows; `None` while the text is the user's own.
    history_pos: Option<usize>,
}

impl TextInput {
    pub fn new(multiline: bool) -> TextInput {
        TextInput {
            multiline,
            ..TextInput::default()
        }
    }

    /// An input holding `text`, with the cursor at its end.
    pub fn with_text(text: &str, multiline: bool) -> TextInput {
        let mut input = TextInput::new(multiline);
        input.insert_str(text);
        input
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set_history(&mut self, newest_first: Vec<String>) {
        self.history = newest_first;
        self.history_pos = None;
    }

    /// The cursor as (line, column), both counted in chars from 0.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        let col = before
            .rsplit('\n')
            .next()
            .map(|l| l.chars().count())
            .unwrap_or(0);
        (line, col)
    }

    /// Inserts text at the cursor (typing or a paste). Line breaks become `\n`.
    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.history_pos = None;
    }

    pub fn key(&mut self, key: KeyEvent) -> Edit {
        let m = key.modifiers;
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);
        let shift = m.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter if (shift || alt) && self.multiline => self.insert_str("\n"),
            KeyCode::Enter => return Edit::Submit,
            KeyCode::Char('j') if ctrl => {
                if !self.multiline {
                    return Edit::Ignored;
                }
                self.insert_str("\n");
            }
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.delete(start, self.cursor);
            }
            KeyCode::Char('k') if ctrl => {
                let end = self.line_end();
                self.delete(self.cursor, end);
            }
            KeyCode::Char('b') if alt => self.cursor = self.word_left(),
            KeyCode::Char('f') if alt => self.cursor = self.word_right(),
            KeyCode::Char(_) if ctrl => return Edit::Ignored,
            KeyCode::Char(c) => self.insert_str(c.encode_utf8(&mut [0u8; 4])),
            KeyCode::Backspace if alt => {
                let start = self.word_left();
                self.delete(start, self.cursor);
            }
            KeyCode::Backspace => {
                if let Some(prev) = self.prev_boundary(self.cursor) {
                    self.delete(prev, self.cursor);
                }
            }
            KeyCode::Delete => {
                if let Some(next) = self.next_boundary(self.cursor) {
                    self.delete(self.cursor, next);
                }
            }
            KeyCode::Left if alt || ctrl => self.cursor = self.word_left(),
            KeyCode::Right if alt || ctrl => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.prev_boundary(self.cursor).unwrap_or(0),
            KeyCode::Right => {
                self.cursor = self.next_boundary(self.cursor).unwrap_or(self.text.len())
            }
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up if self.text.is_empty() || self.history_pos.is_some() => {
                self.history_step(true)
            }
            KeyCode::Down if self.history_pos.is_some() => self.history_step(false),
            KeyCode::Up => self.move_line(-1),
            KeyCode::Down => self.move_line(1),
            _ => return Edit::Ignored,
        }
        Edit::Handled
    }

    fn delete(&mut self, from: usize, to: usize) {
        if from < to {
            self.text.replace_range(from..to, "");
            self.cursor = from;
            self.history_pos = None;
        }
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// Start of the word before the cursor (skips the gap first), like readline.
    fn word_left(&self) -> usize {
        let before: Vec<(usize, char)> = self.text[..self.cursor].char_indices().collect();
        let mut i = before.len();
        while i > 0 && !before[i - 1].1.is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && before[i - 1].1.is_alphanumeric() {
            i -= 1;
        }
        before.get(i).map_or(0, |(b, _)| *b)
    }

    /// End of the word after the cursor (skips the gap first), like readline.
    fn word_right(&self) -> usize {
        let mut chars = self.text[self.cursor..].char_indices().peekable();
        while chars.next_if(|(_, c)| !c.is_alphanumeric()).is_some() {}
        while chars.next_if(|(_, c)| c.is_alphanumeric()).is_some() {}
        chars
            .peek()
            .map_or(self.text.len(), |(i, _)| self.cursor + i)
    }

    fn move_line(&mut self, delta: isize) {
        let (line, col) = self.cursor_line_col();
        let lines: Vec<&str> = self.text.split('\n').collect();
        let target = line as isize + delta;
        if target < 0 || target >= lines.len() as isize {
            return;
        }
        let target = target as usize;
        let start: usize = lines[..target].iter().map(|l| l.len() + 1).sum();
        let within: usize = lines[target].chars().take(col).map(char::len_utf8).sum();
        self.cursor = start + within;
    }

    /// `older`: one entry further back; otherwise one entry forward, and past the
    /// newest entry the input is empty again.
    fn history_step(&mut self, older: bool) {
        let next = match (self.history_pos, older) {
            (None, true) if !self.history.is_empty() => Some(0),
            (None, _) => return,
            (Some(i), true) => Some((i + 1).min(self.history.len() - 1)),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        self.text = next.map(|i| self.history[i].clone()).unwrap_or_default();
        self.cursor = self.text.len();
        self.history_pos = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};

    fn press(input: &mut TextInput, code: K, mods: M) -> Edit {
        input.key(KeyEvent::new(code, mods))
    }

    fn typed(s: &str, multiline: bool) -> TextInput {
        let mut input = TextInput::new(multiline);
        for c in s.chars() {
            assert_eq!(press(&mut input, K::Char(c), M::NONE), Edit::Handled);
        }
        input
    }

    #[test]
    fn typing_and_deleting_turkish_text() {
        let mut t = typed("şişli", false);
        assert_eq!(t.text(), "şişli");
        press(&mut t, K::Backspace, M::NONE);
        assert_eq!(t.text(), "şişl");
        press(&mut t, K::Left, M::NONE);
        press(&mut t, K::Left, M::NONE);
        press(&mut t, K::Delete, M::NONE);
        assert_eq!(t.text(), "şil");
        assert_eq!(t.cursor_line_col(), (0, 2));
    }

    #[test]
    fn enter_submits_and_other_owner_keys_are_ignored() {
        let mut t = typed("fix it", true);
        assert_eq!(press(&mut t, K::Enter, M::NONE), Edit::Submit);
        assert_eq!(press(&mut t, K::Esc, M::NONE), Edit::Ignored);
        assert_eq!(press(&mut t, K::Tab, M::NONE), Edit::Ignored);
        assert_eq!(press(&mut t, K::Char('o'), M::CONTROL), Edit::Ignored);
        assert_eq!(press(&mut t, K::Char('p'), M::CONTROL), Edit::Ignored);
        assert_eq!(t.text(), "fix it");
    }

    #[test]
    fn newline_keys_only_work_in_a_multiline_input() {
        let mut t = typed("a", true);
        press(&mut t, K::Enter, M::SHIFT);
        press(&mut t, K::Char('b'), M::NONE);
        press(&mut t, K::Enter, M::ALT);
        press(&mut t, K::Char('c'), M::NONE);
        press(&mut t, K::Char('j'), M::CONTROL);
        assert_eq!(t.text(), "a\nb\nc\n");
        let mut one = typed("a", false);
        assert_eq!(press(&mut one, K::Enter, M::SHIFT), Edit::Submit);
        assert_eq!(press(&mut one, K::Char('j'), M::CONTROL), Edit::Ignored);
        assert_eq!(one.text(), "a");
    }

    #[test]
    fn line_start_end_and_kills_stay_on_the_current_line() {
        let mut t = TextInput::with_text("first\nsecond line", true);
        press(&mut t, K::Char('a'), M::CONTROL);
        assert_eq!(t.cursor_line_col(), (1, 0));
        press(&mut t, K::Char('k'), M::CONTROL);
        assert_eq!(t.text(), "first\n");
        let mut t = TextInput::with_text("first\nsecond line", true);
        press(&mut t, K::Left, M::NONE);
        press(&mut t, K::Char('u'), M::CONTROL);
        assert_eq!(t.text(), "first\ne");
        press(&mut t, K::Char('a'), M::CONTROL);
        press(&mut t, K::Char('e'), M::CONTROL);
        assert_eq!(t.cursor_line_col(), (1, 1));
        press(&mut t, K::Home, M::NONE);
        assert_eq!(t.cursor_line_col(), (1, 0));
        press(&mut t, K::End, M::NONE);
        assert_eq!(t.cursor_line_col(), (1, 1));
    }

    #[test]
    fn words_move_and_delete_like_readline() {
        let mut t = TextInput::with_text("fix the login-redirect", false);
        press(&mut t, K::Left, M::ALT);
        assert_eq!(t.cursor_line_col(), (0, 14));
        press(&mut t, K::Char('b'), M::ALT);
        assert_eq!(t.cursor_line_col(), (0, 8));
        press(&mut t, K::Right, M::ALT);
        assert_eq!(t.cursor_line_col(), (0, 13));
        press(&mut t, K::Char('f'), M::ALT);
        assert_eq!(t.cursor_line_col(), (0, 22));
        press(&mut t, K::Backspace, M::ALT);
        assert_eq!(t.text(), "fix the login-");
        press(&mut t, K::Backspace, M::ALT);
        assert_eq!(t.text(), "fix the ");
    }

    #[test]
    fn up_and_down_move_between_lines_keeping_the_column() {
        let mut t = TextInput::with_text("abcdef\nxy\nlonger line", true);
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.cursor_line_col(), (1, 2), "clamped to the shorter line");
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.cursor_line_col(), (0, 2));
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.cursor_line_col(), (0, 2), "stops at the first line");
        press(&mut t, K::Down, M::NONE);
        press(&mut t, K::Down, M::NONE);
        assert_eq!(t.cursor_line_col(), (2, 2));
    }

    #[test]
    fn up_in_an_empty_input_walks_the_history_and_down_comes_back() {
        let mut t = TextInput::new(true);
        t.set_history(vec!["newest".into(), "older".into()]);
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.text(), "newest");
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.text(), "older");
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.text(), "older", "stops at the oldest");
        press(&mut t, K::Down, M::NONE);
        assert_eq!(t.text(), "newest");
        press(&mut t, K::Down, M::NONE);
        assert_eq!(t.text(), "", "past the newest the input is empty again");
    }

    #[test]
    fn editing_a_history_entry_makes_it_the_users_own_text() {
        let mut t = TextInput::new(false);
        t.set_history(vec!["one".into(), "two".into()]);
        press(&mut t, K::Up, M::NONE);
        press(&mut t, K::Char('!'), M::NONE);
        press(&mut t, K::Up, M::NONE);
        assert_eq!(t.text(), "one!", "Up no longer walks the history");
        let mut empty = TextInput::new(false);
        press(&mut empty, K::Up, M::NONE);
        assert_eq!(empty.text(), "", "no history, nothing happens");
    }

    #[test]
    fn a_paste_keeps_its_lines_and_normalizes_line_breaks() {
        let mut t = typed("> ", false);
        t.insert_str("one\r\ntwo\rthree");
        assert_eq!(t.text(), "> one\ntwo\nthree");
        assert_eq!(t.cursor_line_col(), (2, 5));
    }
}
