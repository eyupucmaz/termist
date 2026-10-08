//! Ayna on screen: a folder's diff (`g`), the whole branch since it left its base or
//! only the uncommitted, its files marked reviewed with `Ctrl+r`.
use super::DiffView;
use super::view::{Shown, draw as draw_diff};
use crate::app::App;
use ratatui::Frame;
use ratatui::layout::Rect;
use std::collections::BTreeSet;
use std::path::PathBuf;
use termist_core::github::DiffFile;
use termist_core::{DiffMode, LocalDiffData, ReadState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalView {
    /// The folder asked for: a card's worktree or folder.
    pub path: PathBuf,
    pub mode: DiffMode,
    pub view: DiffView,
    /// What the daemon last said; `diff` stays the last one read when a read fails.
    pub state: ReadState,
    pub diff: Option<LocalDiffData>,
    /// How many diffs came, so the drawn lines of an older one are not kept.
    pub reads: u64,
}

impl LocalView {
    pub fn new(path: PathBuf) -> LocalView {
        let mut view = DiffView::new(None);
        view.local = true;
        LocalView {
            path,
            mode: DiffMode::Branch,
            view,
            state: ReadState::Reading,
            diff: None,
            reads: 0,
        }
    }

    pub fn files(&self) -> Option<&[DiffFile]> {
        self.diff.as_ref().map(|d| d.files.as_slice())
    }

    /// What the daemon said about this folder in this mode: the open file stays when it
    /// is still there, else the first not reviewed.
    pub fn arrived(&mut self, state: ReadState, diff: Option<LocalDiffData>) {
        self.state = state;
        if let Some(diff) = diff {
            self.view.settle(&diff.files);
            self.diff = Some(diff);
            self.reads += 1;
        }
    }

    /// `u`: the other mode, read anew.
    pub fn flip_mode(&mut self) {
        self.mode = match self.mode {
            DiffMode::Branch => DiffMode::Uncommitted,
            DiffMode::Uncommitted => DiffMode::Branch,
        };
        self.state = ReadState::Reading;
        self.diff = None;
        self.reads += 1;
    }
}

/// `⎇ fix-login · since origin/main · 3 files +60 −28 ●`.
pub fn title(d: &LocalDiffData, mode: DiffMode) -> String {
    let what = match (mode, d.base.strip_prefix("HEAD ")) {
        (DiffMode::Uncommitted, _) => "uncommitted".to_string(),
        // The whole branch could not be: why, after what is shown instead.
        (DiffMode::Branch, Some(why)) => format!("uncommitted {why}"),
        (DiffMode::Branch, None) => format!("since {}", d.base),
    };
    let (added, removed) = d
        .files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.additions, r + f.deletions));
    let files = match d.files.len() as u32 + d.more {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    let dirty = if d.dirty { " ●" } else { "" };
    format!(
        " ⎇ {} · {what} · {files} +{added} −{removed}{dirty}",
        d.head
    )
}

/// What the diff view draws of a folder.
pub fn shown<'a>(l: &'a LocalView, none: &'a BTreeSet<String>) -> Shown<'a> {
    use std::hash::{Hash, Hasher};
    let mut id = std::collections::hash_map::DefaultHasher::new();
    (&l.path, l.mode, l.reads).hash(&mut id);
    let waiting = match &l.state {
        ReadState::Failed(why) => why.clone(),
        _ => "Reading the diff…".to_string(),
    };
    let nothing = match (l.mode, &l.diff) {
        (DiffMode::Uncommitted, _) => "No uncommitted changes · u: the whole branch".to_string(),
        (DiffMode::Branch, Some(d)) => format!("No changes since {}", d.base),
        (DiffMode::Branch, None) => String::new(),
    };
    Shown {
        id: id.finish(),
        files: l.files(),
        more: l.diff.as_ref().map_or(0, |d| d.more),
        threads: &[],
        marked: none,
        title: l
            .diff
            .as_ref()
            .map(|d| title(d, l.mode))
            .unwrap_or_else(|| format!(" {}", l.path.display())),
        badges: vec![],
        seen: "reviewed",
        failed: matches!(l.state, ReadState::Failed(_)),
        waiting,
        nothing,
        elsewhere: "",
    }
}

pub fn draw(f: &mut Frame, app: &App, l: &LocalView, area: Rect) {
    let none = BTreeSet::new();
    draw_diff(f, app, &shown(l, &none), &l.view, area);
}

/// The footer while a folder's diff is up; why the newest read failed comes first.
pub fn hint(app: &App, l: &LocalView) -> String {
    if l.view.typing {
        return "type to search the paths · ↑/↓ choose · Enter keep · Esc clear".into();
    }
    let other = match app.config.diff.layout {
        termist_core::config::DiffLayout::Unified => "split",
        termist_core::config::DiffLayout::Split => "unified",
    };
    let mode = match l.mode {
        DiffMode::Branch => "uncommitted",
        DiffMode::Uncommitted => "whole branch",
    };
    let failed = match &l.state {
        ReadState::Failed(why) if l.diff.is_some() => format!("{why} · "),
        _ => String::new(),
    };
    format!(
        "{failed}Tab panel · J/K file · {{/}} hunk · u {mode} · ^R reviewed · R reload · s {other} · Esc back"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::github::{Patch, Viewed};

    fn file(path: &str, additions: u32, deletions: u32) -> DiffFile {
        DiffFile {
            path: path.into(),
            previous: None,
            change: 'M',
            additions,
            deletions,
            viewed: Viewed::Unviewed,
            patch: Patch::Text(String::new()),
            url: String::new(),
        }
    }

    #[test]
    fn the_title_says_the_branch_what_it_is_measured_from_and_how_much() {
        let mut d = LocalDiffData {
            head: "fix-login".into(),
            base: "origin/main".into(),
            dirty: true,
            files: vec![file("a.rs", 50, 20), file("b.rs", 10, 8)],
            more: 1,
        };
        assert_eq!(
            title(&d, DiffMode::Branch),
            " ⎇ fix-login · since origin/main · 3 files +60 −28 ●"
        );
        d.base = "HEAD".into();
        d.files.truncate(1);
        (d.more, d.dirty) = (0, false);
        assert_eq!(
            title(&d, DiffMode::Uncommitted),
            " ⎇ fix-login · uncommitted · 1 file +50 −20"
        );
        d.base = "HEAD (no merge base with origin/main)".into();
        assert_eq!(
            title(&d, DiffMode::Branch),
            " ⎇ fix-login · uncommitted (no merge base with origin/main) · 1 file +50 −20"
        );
    }

    #[test]
    fn u_reads_the_other_mode_anew() {
        let mut l = LocalView::new("/w/site".into());
        l.arrived(
            ReadState::Ready,
            Some(LocalDiffData {
                files: vec![file("a.rs", 1, 0), file("b.rs", 1, 0)],
                ..LocalDiffData::default()
            }),
        );
        assert_eq!(l.view.file.as_deref(), Some("a.rs"));
        l.view.file = Some("b.rs".into());
        l.flip_mode();
        assert_eq!((l.mode, l.diff.is_none()), (DiffMode::Uncommitted, true));
        l.arrived(
            ReadState::Ready,
            Some(LocalDiffData {
                files: vec![file("a.rs", 1, 0), file("b.rs", 1, 0)],
                ..LocalDiffData::default()
            }),
        );
        assert_eq!(
            l.view.file.as_deref(),
            Some("b.rs"),
            "there in this mode too"
        );
        l.arrived(
            ReadState::Ready,
            Some(LocalDiffData {
                files: vec![file("a.rs", 1, 0)],
                ..LocalDiffData::default()
            }),
        );
        assert_eq!(l.view.file.as_deref(), Some("a.rs"), "b.rs is gone: on");
    }
}
