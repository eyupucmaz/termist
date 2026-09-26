//! A list to choose from, optionally narrowed by typing (the letters of the query must
//! appear in order, like a simple fzf). It knows nothing about the screen.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key did to the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The highlight or the query changed.
    Handled,
    /// Enter on a highlighted item.
    Chosen,
    /// Not a picker key (Esc, Tab, digits in a list without a query, …).
    Ignored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListPicker<T> {
    items: Vec<T>,
    labels: Vec<String>,
    /// `Some` for a list you can type into; `j`/`k` then are letters, not moves.
    query: Option<String>,
    /// Indices into `items` that match the query, in list order.
    visible: Vec<usize>,
    /// Position in `visible`.
    highlight: usize,
}

/// True when the letters of `query` appear in `label` in order, ignoring case.
pub fn matches(query: &str, label: &str) -> bool {
    let mut label = label.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| label.any(|l| l == q))
}

impl<T> ListPicker<T> {
    /// `label` is the text the query is matched against.
    pub fn new(items: Vec<T>, label: impl Fn(&T) -> String, filterable: bool) -> ListPicker<T> {
        let labels = items.iter().map(&label).collect();
        let mut picker = ListPicker {
            items,
            labels,
            query: filterable.then(String::new),
            visible: vec![],
            highlight: 0,
        };
        picker.refilter();
        picker
    }

    /// Replaces the items (a newer list arrived) and keeps the highlight on the same
    /// position, clamped to the new length.
    pub fn set_items(&mut self, items: Vec<T>, label: impl Fn(&T) -> String) {
        self.labels = items.iter().map(label).collect();
        self.items = items;
        self.refilter();
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// The highlighted item.
    pub fn selected(&self) -> Option<&T> {
        self.selected_index().map(|i| &self.items[i])
    }

    /// Index into `items` of the highlighted item.
    pub fn selected_index(&self) -> Option<usize> {
        self.visible.get(self.highlight).copied()
    }

    /// Highlights item `index` if it is visible.
    pub fn select_index(&mut self, index: usize) {
        if let Some(pos) = self.visible.iter().position(|&i| i == index) {
            self.highlight = pos;
        }
    }

    /// The visible items in order: (index into `items`, item, highlighted).
    pub fn visible(&self) -> impl Iterator<Item = (usize, &T, bool)> {
        self.visible
            .iter()
            .enumerate()
            .map(|(pos, &i)| (i, &self.items[i], pos == self.highlight))
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// Position of the highlight among the visible items.
    pub fn highlight(&self) -> usize {
        self.highlight
    }

    pub fn key(&mut self, key: KeyEvent) -> Pick {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let typing = self.query.is_some();
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('p') if ctrl && typing => self.step(-1),
            KeyCode::Char('j') if !typing && !ctrl => self.step(1),
            KeyCode::Char('k') if !typing && !ctrl => self.step(-1),
            KeyCode::Enter if self.selected_index().is_some() => return Pick::Chosen,
            KeyCode::Backspace if typing => {
                if let Some(q) = self.query.as_mut() {
                    q.pop();
                }
                self.refilter();
            }
            KeyCode::Char(c) if typing && !ctrl && !alt => {
                if let Some(q) = self.query.as_mut() {
                    q.push(c);
                }
                self.highlight = 0;
                self.refilter();
            }
            _ => return Pick::Ignored,
        }
        Pick::Handled
    }

    /// Moves the highlight, stopping at either end.
    fn step(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1) as isize;
        self.highlight = (self.highlight as isize + delta).clamp(0, last) as usize;
    }

    fn refilter(&mut self) {
        let query = self.query.clone().unwrap_or_default();
        self.visible = (0..self.items.len())
            .filter(|&i| matches(&query, &self.labels[i]))
            .collect();
        self.highlight = self.highlight.min(self.visible.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};

    fn press(p: &mut ListPicker<&'static str>, code: K) -> Pick {
        p.key(KeyEvent::new(code, M::NONE))
    }

    fn names(p: &ListPicker<&'static str>) -> Vec<&'static str> {
        p.visible().map(|(_, s, _)| *s).collect()
    }

    #[test]
    fn letters_must_appear_in_order_ignoring_case() {
        assert!(matches("oapi", "orbit-api"));
        assert!(matches("ORB", "orbit-api"));
        assert!(matches("", "anything"));
        assert!(!matches("ipa", "orbit-api"));
        assert!(matches("şi", "Şişli"));
    }

    #[test]
    fn typing_narrows_the_list_and_backspace_widens_it() {
        let mut p = ListPicker::new(
            vec!["orbit-api", "orbit-web", "notes"],
            |s| s.to_string(),
            true,
        );
        press(&mut p, K::Char('w'));
        assert_eq!(names(&p), vec!["orbit-web"]);
        assert_eq!(p.selected(), Some(&"orbit-web"));
        assert_eq!(p.query(), Some("w"));
        press(&mut p, K::Backspace);
        assert_eq!(names(&p).len(), 3);
        press(&mut p, K::Char('z'));
        assert_eq!(p.selected(), None);
        assert_eq!(press(&mut p, K::Enter), Pick::Ignored, "nothing to choose");
    }

    #[test]
    fn moving_stops_at_both_ends() {
        let mut p = ListPicker::new(vec!["a", "b", "c"], |s| s.to_string(), false);
        press(&mut p, K::Up);
        assert_eq!(p.selected(), Some(&"a"));
        press(&mut p, K::Char('j'));
        press(&mut p, K::Down);
        press(&mut p, K::Down);
        assert_eq!(p.selected(), Some(&"c"));
        press(&mut p, K::Char('k'));
        assert_eq!(p.selected(), Some(&"b"));
        assert_eq!(press(&mut p, K::Enter), Pick::Chosen);
        assert_eq!(press(&mut p, K::Esc), Pick::Ignored);
        assert_eq!(press(&mut p, K::Char('1')), Pick::Ignored);
    }

    #[test]
    fn in_a_list_you_type_into_j_and_k_are_letters() {
        let mut p = ListPicker::new(vec!["jobs", "kit"], |s| s.to_string(), true);
        press(&mut p, K::Char('k'));
        assert_eq!(names(&p), vec!["kit"]);
        let mut p = ListPicker::new(vec!["a", "b"], |s| s.to_string(), true);
        p.key(KeyEvent::new(K::Char('n'), M::CONTROL));
        assert_eq!(p.selected(), Some(&"b"));
        p.key(KeyEvent::new(K::Char('p'), M::CONTROL));
        assert_eq!(p.selected(), Some(&"a"));
    }

    #[test]
    fn new_items_keep_the_highlight_in_range() {
        let mut p = ListPicker::new(vec!["a", "b", "c"], |s| s.to_string(), false);
        p.select_index(2);
        p.set_items(vec!["a", "b"], |s| s.to_string());
        assert_eq!(p.selected(), Some(&"b"));
        p.set_items(vec![], |s: &&str| s.to_string());
        assert_eq!(p.selected(), None);
    }
}
