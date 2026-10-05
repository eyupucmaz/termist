//! Mercek: a pull request's diff, its file tree beside one file's changes. This module
//! keeps the state and the keys; `view` draws it.
pub mod render;
pub mod tree;
pub mod words;

use super::PrAction;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};
use termist_core::github::{DiffFile, PrDiff, PrRef, Viewed};
use tree::{Node, TreeRow};

/// How far `←` and `→` move a long line.
pub const SIDEWAYS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Panel {
    Tree,
    #[default]
    Diff,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffView {
    /// Where the keys go; on a narrow screen, the one panel shown.
    pub panel: Panel,
    /// The file shown, by path, so it stays through a new head. `None` until the diff
    /// comes: then the first file not viewed.
    pub file: Option<String>,
    /// The highlighted row of the tree.
    pub cursor: usize,
    /// Folded folders, by path.
    pub folded: HashSet<String>,
    /// The `/` search over paths.
    pub query: String,
    /// Keys go into `query`.
    pub typing: bool,
    /// The first line of the diff on screen, and how far long lines are moved left.
    pub scroll: usize,
    pub hscroll: usize,
    /// Threads unfolded, by id.
    pub opened: HashSet<String>,
    /// Viewed states asked of GitHub and not yet seen in a diff from it, by path.
    pub pending: HashMap<String, Viewed>,
}

/// Where the last frame put the diff's parts, for the keys and the mouse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffArea {
    /// The tree's rows on screen, and the index of the first one; empty when hidden.
    pub tree: Rect,
    pub tree_first: usize,
    /// The diff's lines on screen.
    pub body: Rect,
    /// How far the diff scrolls, and a page of it.
    pub end: usize,
    pub page: usize,
    /// The line of each hunk header.
    pub hunks: Vec<usize>,
    /// Each thread's first line and its id.
    pub threads: Vec<(usize, String)>,
}

/// What a key in the diff asks of the PR view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffAction {
    /// Back to the pull request, on this file.
    Back(Option<String>),
    Pr(PrAction),
}

impl DiffView {
    /// The diff opened on `file`, or on the first file not viewed when `None`.
    pub fn new(file: Option<String>) -> DiffView {
        DiffView {
            file,
            ..DiffView::default()
        }
    }

    /// `f` viewed or not, as asked here before GitHub says so.
    pub fn viewed(&self, f: &DiffFile) -> Viewed {
        self.pending.get(&f.path).copied().unwrap_or(f.viewed)
    }

    pub fn rows(&self, diff: &PrDiff) -> Vec<TreeRow> {
        tree::rows(&diff.files, &self.folded, &self.query)
    }

    /// The file shown, as an index into `diff.files`.
    pub fn index(&self, diff: &PrDiff) -> Option<usize> {
        let path = self.file.as_deref()?;
        diff.files.iter().position(|f| f.path == path)
    }

    /// A new diff came: what GitHub now says replaces what was asked, and a file that
    /// is gone (or none yet) becomes the first one not viewed.
    pub fn settle(&mut self, diff: &PrDiff) {
        self.pending.retain(|path, want| {
            diff.files
                .iter()
                .find(|f| f.path == *path)
                .is_some_and(|f| f.viewed != *want)
        });
        match self.index(diff) {
            Some(i) => self.follow(i, diff),
            None => {
                let order = tree::order(&diff.files);
                let first = order
                    .iter()
                    .find(|i| self.viewed(&diff.files[**i]) != Viewed::Viewed)
                    .or(order.first());
                if let Some(&i) = first {
                    self.show(i, diff);
                }
            }
        }
    }

    /// The tree's cursor on file `index`, when its row is shown.
    fn follow(&mut self, index: usize, diff: &PrDiff) {
        if let Some(row) = self.rows(diff).iter().position(|r| r.file() == Some(index)) {
            self.cursor = row;
        }
    }

    /// Shows file `index` from its first line, the tree's cursor on it.
    fn show(&mut self, index: usize, diff: &PrDiff) {
        self.file = Some(diff.files[index].path.clone());
        self.scroll = 0;
        self.hscroll = 0;
        self.follow(index, diff);
    }

    /// The next (or previous) file in the tree's order from the one shown.
    fn step_file(&mut self, diff: &PrDiff, delta: isize) {
        let order = tree::order(&diff.files);
        if order.is_empty() {
            return;
        }
        let at = self
            .index(diff)
            .and_then(|i| order.iter().position(|x| *x == i))
            .unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, order.len() as isize - 1) as usize;
        self.show(order[next], diff);
    }

    /// The first file after the one shown, round the end, that is not viewed.
    fn next_unviewed(&mut self, diff: &PrDiff) {
        let order = tree::order(&diff.files);
        let at = self
            .index(diff)
            .and_then(|i| order.iter().position(|x| *x == i))
            .unwrap_or(0);
        let next = (1..order.len())
            .map(|k| order[(at + k) % order.len()])
            .find(|i| self.viewed(&diff.files[*i]) != Viewed::Viewed);
        if let Some(i) = next {
            self.show(i, diff);
        }
    }

    fn enter_row(&mut self, row: &TreeRow, diff: &PrDiff) {
        match &row.node {
            Node::File { index, .. } => {
                self.show(*index, diff);
                self.panel = Panel::Diff;
            }
            Node::Dir { path, .. } => {
                if !self.folded.remove(path) {
                    self.folded.insert(path.clone());
                }
            }
        }
    }

    pub fn key(
        &mut self,
        key: KeyEvent,
        pr: PrRef,
        diff: Option<&PrDiff>,
        area: &DiffArea,
    ) -> Option<DiffAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.typing {
            match key.code {
                KeyCode::Esc => {
                    self.typing = false;
                    self.query.clear();
                }
                KeyCode::Enter => self.typing = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Down => self.cursor += 1,
                KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Char(c) if !ctrl => {
                    self.query.push(c);
                    self.cursor = 0;
                }
                _ => {}
            }
            self.clamp(diff);
            return None;
        }
        if key.code == KeyCode::Esc {
            if !self.query.is_empty() {
                self.query.clear();
                self.clamp(diff);
                return None;
            }
            return Some(DiffAction::Back(self.file.clone()));
        }
        let diff = diff?;
        let half = (area.page / 2).max(1);
        let page = area.page.max(1);
        let rows = self.rows(diff);
        let tree = self.panel == Panel::Tree;
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.panel = if tree { Panel::Diff } else { Panel::Tree };
            }
            KeyCode::Char('/') => {
                self.typing = true;
                self.panel = Panel::Tree;
            }
            KeyCode::Char('J') => self.step_file(diff, 1),
            KeyCode::Char('K') => self.step_file(diff, -1),
            KeyCode::Char('r') if ctrl => {
                let path = self.file.clone()?;
                let f = diff.files.iter().find(|f| f.path == path)?;
                let viewed = self.viewed(f) != Viewed::Viewed;
                let now = if viewed {
                    Viewed::Viewed
                } else {
                    Viewed::Unviewed
                };
                self.pending.insert(path.clone(), now);
                if viewed {
                    self.next_unviewed(diff);
                }
                return Some(DiffAction::Pr(PrAction::Viewed { pr, path, viewed }));
            }
            KeyCode::Char('s') => return Some(DiffAction::Pr(PrAction::FlipLayout)),
            KeyCode::Char('b') => {
                let i = self.index(diff)?;
                return Some(DiffAction::Pr(PrAction::Browser(diff.files[i].url.clone())));
            }
            KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(SIDEWAYS),
            KeyCode::Right => self.hscroll += SIDEWAYS,
            _ if tree => match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.cursor += 1,
                KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Char('d') if ctrl => self.cursor += half,
                KeyCode::Char('u') if ctrl => self.cursor = self.cursor.saturating_sub(half),
                KeyCode::PageDown => self.cursor += page,
                KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(page),
                KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
                KeyCode::Char('G') | KeyCode::End => self.cursor = rows.len().saturating_sub(1),
                KeyCode::Enter => {
                    let row = rows.get(self.cursor)?.clone();
                    self.enter_row(&row, diff);
                }
                KeyCode::Char('l') => match rows.get(self.cursor).map(|r| r.node.clone()) {
                    Some(Node::Dir { path, .. }) => {
                        self.folded.remove(&path);
                    }
                    Some(Node::File { index, .. }) => {
                        self.show(index, diff);
                        self.panel = Panel::Diff;
                    }
                    None => {}
                },
                KeyCode::Char('h') => {
                    if let Some(Node::Dir { path, .. }) = rows.get(self.cursor).map(|r| &r.node) {
                        self.folded.insert(path.clone());
                    }
                }
                _ => {}
            },
            KeyCode::Char('j') | KeyCode::Down => self.scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => self.scroll += half,
            KeyCode::Char('u') if ctrl => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::PageDown => self.scroll += page,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = area.end,
            KeyCode::Char('l') => self.hscroll += SIDEWAYS,
            KeyCode::Char('h') => self.hscroll = self.hscroll.saturating_sub(SIDEWAYS),
            KeyCode::Char('}') => {
                if let Some(&line) = area.hunks.iter().find(|l| **l > self.scroll) {
                    self.scroll = line;
                }
            }
            KeyCode::Char('{') => {
                if let Some(&line) = area.hunks.iter().rev().find(|l| **l < self.scroll) {
                    self.scroll = line;
                }
            }
            KeyCode::Char('n') => {
                if let Some((line, _)) = area.threads.iter().find(|(l, _)| *l > self.scroll) {
                    self.scroll = *line;
                }
            }
            KeyCode::Char('N') => {
                if let Some((line, _)) = area.threads.iter().rev().find(|(l, _)| *l < self.scroll) {
                    self.scroll = *line;
                }
            }
            KeyCode::Enter => {
                if let Some((_, id)) = area.threads.iter().find(|(l, _)| *l >= self.scroll) {
                    self.toggle(id);
                }
            }
            _ => {}
        }
        self.scroll = self.scroll.min(area.end);
        self.clamp(Some(diff));
        None
    }

    /// Unfolds a folded thread, folds an open one.
    pub fn toggle(&mut self, id: &str) {
        if !self.opened.remove(id) {
            self.opened.insert(id.to_string());
        }
    }

    /// Keeps the tree's cursor on a row.
    fn clamp(&mut self, diff: Option<&PrDiff>) {
        let n = diff.map_or(0, |d| self.rows(d).len());
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::tree::tests::file;
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};
    use termist_core::github::RepoId;

    fn k(code: K) -> KeyEvent {
        KeyEvent::new(code, M::NONE)
    }

    fn pr() -> PrRef {
        PrRef {
            repo: RepoId(1),
            number: 212,
        }
    }

    /// In the tree's order: `src/api/client.ts`, `src/search/DealerFilter.tsx`,
    /// `src/App.tsx`, `README.md`; DealerFilter already viewed.
    fn diff() -> PrDiff {
        let mut files: Vec<DiffFile> = [
            "README.md",
            "src/search/DealerFilter.tsx",
            "src/App.tsx",
            "src/api/client.ts",
        ]
        .into_iter()
        .map(file)
        .collect();
        files[1].viewed = Viewed::Viewed;
        PrDiff {
            head_oid: "h1".into(),
            files,
            more: 0,
        }
    }

    fn area() -> DiffArea {
        DiffArea {
            end: 50,
            page: 10,
            hunks: vec![0, 12, 30],
            threads: vec![(4, "T1".into()), (20, "T2".into())],
            ..DiffArea::default()
        }
    }

    fn key(v: &mut DiffView, code: K) -> Option<DiffAction> {
        v.key(k(code), pr(), Some(&diff()), &area())
    }

    #[test]
    fn a_new_diff_opens_on_the_first_file_not_viewed() {
        let mut v = DiffView::new(None);
        v.settle(&diff());
        assert_eq!(v.file.as_deref(), Some("src/api/client.ts"));
        let mut v = DiffView::new(Some("src/App.tsx".into()));
        v.settle(&diff());
        assert_eq!(
            v.file.as_deref(),
            Some("src/App.tsx"),
            "a file asked for stays"
        );
        assert_eq!(v.cursor, 5, "the cursor on its row");
    }

    #[test]
    fn capital_j_and_k_go_through_the_files_in_tree_order() {
        let mut v = DiffView::new(Some("src/api/client.ts".into()));
        v.scroll = 9;
        key(&mut v, K::Char('J'));
        assert_eq!(v.file.as_deref(), Some("src/search/DealerFilter.tsx"));
        assert_eq!(v.scroll, 0);
        key(&mut v, K::Char('K'));
        key(&mut v, K::Char('K'));
        assert_eq!(
            v.file.as_deref(),
            Some("src/api/client.ts"),
            "held at the first"
        );
    }

    #[test]
    fn hunks_and_threads_are_reached_from_where_the_screen_is() {
        let mut v = DiffView::new(Some("src/App.tsx".into()));
        key(&mut v, K::Char('}'));
        assert_eq!(v.scroll, 12);
        key(&mut v, K::Char('n'));
        assert_eq!(v.scroll, 20);
        key(&mut v, K::Char('{'));
        assert_eq!(v.scroll, 12);
        key(&mut v, K::Char('N'));
        assert_eq!(v.scroll, 4);
        key(&mut v, K::Enter);
        assert!(v.opened.contains("T1"));
        key(&mut v, K::Enter);
        assert!(v.opened.is_empty());
        key(&mut v, K::Char('G'));
        assert_eq!(v.scroll, 50);
        key(&mut v, K::Char('j'));
        assert_eq!(v.scroll, 50, "held at the end");
    }

    #[test]
    fn ctrl_r_marks_viewed_and_moves_to_the_next_not_viewed() {
        let mut v = DiffView::new(Some("src/api/client.ts".into()));
        let a = v.key(
            KeyEvent::new(K::Char('r'), M::CONTROL),
            pr(),
            Some(&diff()),
            &area(),
        );
        assert_eq!(
            a,
            Some(DiffAction::Pr(PrAction::Viewed {
                pr: pr(),
                path: "src/api/client.ts".into(),
                viewed: true
            }))
        );
        assert_eq!(
            v.file.as_deref(),
            Some("src/App.tsx"),
            "DealerFilter was viewed already"
        );
        let a = v.key(
            KeyEvent::new(K::Char('r'), M::CONTROL),
            pr(),
            Some(&diff()),
            &area(),
        );
        assert!(matches!(
            a,
            Some(DiffAction::Pr(PrAction::Viewed { viewed: true, .. }))
        ));
        assert_eq!(v.file.as_deref(), Some("README.md"));
        // README is the last not viewed: marking it wraps round to nothing left.
        v.key(
            KeyEvent::new(K::Char('r'), M::CONTROL),
            pr(),
            Some(&diff()),
            &area(),
        );
        assert_eq!(v.file.as_deref(), Some("README.md"));
        // Again on a viewed file: not viewed, and it stays.
        let a = v.key(
            KeyEvent::new(K::Char('r'), M::CONTROL),
            pr(),
            Some(&diff()),
            &area(),
        );
        assert!(matches!(
            a,
            Some(DiffAction::Pr(PrAction::Viewed { viewed: false, .. }))
        ));
        assert_eq!(v.file.as_deref(), Some("README.md"));
    }

    #[test]
    fn what_github_says_replaces_what_was_asked() {
        let mut v = DiffView::new(Some("README.md".into()));
        v.pending.insert("README.md".into(), Viewed::Viewed);
        v.pending.insert("src/App.tsx".into(), Viewed::Viewed);
        let mut d = diff();
        d.files[0].viewed = Viewed::Viewed;
        v.settle(&d);
        assert_eq!(
            v.pending.keys().collect::<Vec<_>>(),
            ["src/App.tsx"],
            "README confirmed, App.tsx still on its way"
        );
    }

    #[test]
    fn the_tree_opens_files_folds_folders_and_searches() {
        let mut v = DiffView::new(Some("README.md".into()));
        v.panel = Panel::Tree;
        v.cursor = 0;
        key(&mut v, K::Char('h'));
        assert!(v.folded.contains("src"));
        assert_eq!(v.rows(&diff()).len(), 2, "src folded, README");
        key(&mut v, K::Enter);
        assert!(v.folded.is_empty());
        key(&mut v, K::Char('j'));
        key(&mut v, K::Char('j'));
        key(&mut v, K::Enter);
        assert_eq!(v.file.as_deref(), Some("src/api/client.ts"));
        assert_eq!(v.panel, Panel::Diff);
        key(&mut v, K::Char('/'));
        for c in "readme".chars() {
            key(&mut v, K::Char(c));
        }
        assert_eq!((v.panel, v.cursor), (Panel::Tree, 0));
        key(&mut v, K::Enter);
        assert!(!v.typing);
        key(&mut v, K::Enter);
        assert_eq!(v.file.as_deref(), Some("README.md"));
        assert_eq!(key(&mut v, K::Esc), None, "Esc clears the search first");
        assert_eq!(
            key(&mut v, K::Esc),
            Some(DiffAction::Back(Some("README.md".into())))
        );
    }

    #[test]
    fn sideways_tab_layout_and_browser() {
        let mut v = DiffView::new(Some("README.md".into()));
        key(&mut v, K::Right);
        key(&mut v, K::Char('l'));
        assert_eq!(v.hscroll, 16);
        key(&mut v, K::Left);
        assert_eq!(v.hscroll, 8);
        key(&mut v, K::Tab);
        assert_eq!(v.panel, Panel::Tree);
        assert_eq!(
            key(&mut v, K::Char('s')),
            Some(DiffAction::Pr(PrAction::FlipLayout))
        );
        assert_eq!(
            key(&mut v, K::Char('b')),
            Some(DiffAction::Pr(PrAction::Browser(
                "https://github.com/acme/site/pull/212/files#diff-README.md".into()
            )))
        );
    }

    #[test]
    fn without_a_diff_only_esc_works() {
        let mut v = DiffView::new(None);
        assert_eq!(v.key(k(K::Char('J')), pr(), None, &area()), None);
        assert_eq!(
            v.key(k(K::Esc), pr(), None, &area()),
            Some(DiffAction::Back(None))
        );
    }
}
