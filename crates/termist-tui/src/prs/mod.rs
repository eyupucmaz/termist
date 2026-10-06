//! The pull request view: a project's open pull requests (the inbox) and one of them
//! whole (the detail). This module keeps its state and keys; `inbox_view` and
//! `detail_view` draw it.
pub mod detail_view;
pub mod diff;
pub mod inbox_view;
pub mod markdown;
pub mod timeline;

use crate::app::App;
use crate::list_picker::matches;
use crate::theme::Theme;
use diff::{DiffAction, DiffArea, DiffView};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use std::collections::HashSet;
use termist_core::AgentStatus;
use termist_core::ProjectId;
use termist_core::github::{
    Checks, CommentKind, GhState, Mergeable, PrDiff, PrRef, PrSummary, RepoPrs, ReviewDecision,
};

/// A project's pull requests as the daemon last sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPrs {
    pub state: GhState,
    pub discovered: u32,
    pub repos: Vec<RepoPrs>,
}

impl Default for ProjectPrs {
    fn default() -> Self {
        ProjectPrs {
            state: GhState::Ok,
            discovered: 0,
            repos: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    /// Asked of you, by login.
    ToReview,
    /// Opened by you.
    Mine,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::All, Filter::ToReview, Filter::Mine];

    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::ToReview,
            Filter::ToReview => Filter::Mine,
            Filter::Mine => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::ToReview => "⇄ to review",
            Filter::Mine => "mine",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Overview,
    Conversation,
    Checks,
    Files,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::Conversation, Tab::Checks, Tab::Files];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Conversation => "Conversation",
            Tab::Checks => "Checks",
            Tab::Files => "Files",
        }
    }

    fn step(self, delta: isize) -> Tab {
        let at = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        Tab::ALL[(at + delta).rem_euclid(Tab::ALL.len() as isize) as usize]
    }
}

/// One pull request open whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub pr: PrRef,
    /// As the list had it; the detail's own head replaces it once read.
    pub summary: PrSummary,
    pub tab: Tab,
    pub scroll: usize,
    /// Threads folded or unfolded against their default: resolved and outdated ones
    /// start folded.
    pub toggled: HashSet<String>,
    /// The highlighted check.
    pub check: usize,
    /// The highlighted file of the Files tab, by path.
    pub file: Option<String>,
    /// The highlighted item of the Conversation tab.
    pub item: usize,
    /// Mercek: the diff, over the pull request.
    pub diff: Option<DiffView>,
}

impl Detail {
    pub fn new(pr: PrRef, summary: PrSummary) -> Detail {
        Detail {
            pr,
            summary,
            tab: Tab::Overview,
            scroll: 0,
            toggled: HashSet::new(),
            check: 0,
            file: None,
            item: 0,
            diff: None,
        }
    }

    /// Opens the diff on `file`; one already read settles it at once.
    fn open_diff(&mut self, file: Option<String>, diff: Option<&PrDiff>) {
        let mut view = DiffView::new(file);
        if let Some(diff) = diff {
            view.settle(diff);
        }
        self.diff = Some(view);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrView {
    /// The project the view was set up for; another tab starts it afresh.
    pub project: Option<ProjectId>,
    pub filter: Filter,
    /// The `/` search.
    pub query: String,
    /// Keys go into `query`.
    pub typing: bool,
    pub selected: Option<PrRef>,
    /// The selection's place among the PRs: where it lands when its PR goes.
    pub index: usize,
    pub detail: Option<Detail>,
}

/// Where the last frame put things, for the keys and the mouse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrLayout {
    /// The inbox rows on screen, and the index of the first one shown.
    pub list: Rect,
    pub first: usize,
    /// The detail body: how far it scrolls, and a page.
    pub end: usize,
    pub page: usize,
    /// The conversation's items: comments, reviews and threads.
    pub items: Vec<Item>,
    /// The checks' links, in the order shown.
    pub checks: Vec<Option<String>>,
    /// The Files tab's paths, in the order shown.
    pub files: Vec<String>,
    pub diff: DiffArea,
    /// The detail's tab names on screen (their columns) and their row.
    pub tabs: Vec<(Tab, u16, u16)>,
    pub tab_row: u16,
    /// The detail's body on screen and the line shown at its top.
    pub body: Rect,
    pub scroll: usize,
}

/// An item of the conversation, from its first line: what the keys can do on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub line: usize,
    /// The thread's id when the item is a thread.
    pub thread: Option<String>,
    /// A thread not resolved: `n` and `N` stop at it.
    pub open: bool,
    pub can_reply: bool,
    pub can_resolve: bool,
    pub resolved: bool,
    /// Your comment there to edit or delete: the item itself, or in a thread the last
    /// one you wrote.
    pub mine: Option<Mine>,
}

/// A comment of yours, and what GitHub lets you do with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mine {
    pub id: String,
    pub kind: CommentKind,
    pub body: String,
    pub can_edit: bool,
    pub can_delete: bool,
}

/// What a key in the view asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrAction {
    /// Back to the grid.
    Close,
    /// A pull request was opened as it was at this `updatedAt`.
    Opened(PrRef, String),
    Browser(String),
    Repos,
    /// Mark a file of the diff viewed on GitHub, or not.
    Viewed {
        pr: PrRef,
        path: String,
        viewed: bool,
    },
    /// The diff the other way: unified or split.
    FlipLayout,
}

/// A line of the inbox list.
#[derive(Clone, Copy, Debug)]
pub enum Row<'a> {
    Repo {
        repo: &'a RepoPrs,
        shown: usize,
    },
    Pr {
        repo: &'a RepoPrs,
        pr: &'a PrSummary,
    },
    /// A repo with nothing to show.
    Nothing {
        repo: &'a RepoPrs,
    },
    /// Open PRs beyond those read.
    More {
        repo: &'a RepoPrs,
        more: u32,
    },
}

impl Row<'_> {
    pub fn pr_ref(&self) -> Option<PrRef> {
        match self {
            Row::Pr { repo, pr } => Some(PrRef {
                repo: repo.repo,
                number: pr.number,
            }),
            _ => None,
        }
    }
}

fn keeps(view: &PrView, repo: &RepoPrs, pr: &PrSummary) -> bool {
    let filter = match view.filter {
        Filter::All => true,
        Filter::ToReview => pr.requested_you,
        Filter::Mine => repo.viewer.as_deref() == Some(pr.author.as_str()),
    };
    filter
        && (view.query.is_empty()
            || matches(
                &view.query,
                &format!("#{} {} {}", pr.number, pr.title, pr.author),
            ))
}

/// The inbox: each repo's heading, then its PRs (drafts last, the rest as GitHub
/// sorted them: last updated first).
pub fn rows<'a>(data: &'a ProjectPrs, view: &PrView) -> Vec<Row<'a>> {
    let mut out = Vec::new();
    for repo in &data.repos {
        let mut prs: Vec<&PrSummary> = repo.prs.iter().filter(|p| keeps(view, repo, p)).collect();
        prs.sort_by_key(|p| p.draft);
        out.push(Row::Repo {
            repo,
            shown: prs.len(),
        });
        if prs.is_empty() {
            out.push(Row::Nothing { repo });
        }
        out.extend(prs.into_iter().map(|pr| Row::Pr { repo, pr }));
        let more = repo.total.saturating_sub(repo.prs.len() as u32);
        if more > 0 && view.filter == Filter::All && view.query.is_empty() {
            out.push(Row::More { repo, more });
        }
    }
    out
}

impl PrView {
    pub fn for_project(project: Option<ProjectId>) -> PrView {
        PrView {
            project,
            ..PrView::default()
        }
    }

    pub fn repair(&mut self, rows: &[Row]) {
        let prs: Vec<PrRef> = rows.iter().filter_map(Row::pr_ref).collect();
        if let Some(i) = self.selected.and_then(|s| prs.iter().position(|p| *p == s)) {
            self.index = i;
            return;
        }
        self.index = self.index.min(prs.len().saturating_sub(1));
        self.selected = prs.get(self.index).copied();
    }

    fn step(&mut self, rows: &[Row], delta: isize) {
        let prs: Vec<PrRef> = rows.iter().filter_map(Row::pr_ref).collect();
        if prs.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| prs.iter().position(|p| *p == s))
            .unwrap_or(0) as isize;
        self.index = at.saturating_add(delta).clamp(0, prs.len() as isize - 1) as usize;
        self.selected = Some(prs[self.index]);
    }

    pub fn selection<'a>(&self, data: &'a ProjectPrs) -> Option<(&'a RepoPrs, &'a PrSummary)> {
        let s = self.selected?;
        let repo = data.repos.iter().find(|r| r.repo == s.repo)?;
        Some((repo, repo.prs.iter().find(|p| p.number == s.number)?))
    }

    /// A key; `diff` is the open pull request's diff, if one was read.
    pub fn key(
        &mut self,
        key: KeyEvent,
        data: &ProjectPrs,
        diff: Option<&PrDiff>,
        layout: &PrLayout,
    ) -> Option<PrAction> {
        if self.detail.is_some() {
            self.detail_key(key, diff, layout)
        } else {
            self.inbox_key(key, data)
        }
    }

    fn inbox_key(&mut self, key: KeyEvent, data: &ProjectPrs) -> Option<PrAction> {
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
                KeyCode::Down => self.step(&rows(data, self), 1),
                KeyCode::Up => self.step(&rows(data, self), -1),
                KeyCode::Char(c) if !ctrl => self.query.push(c),
                _ => {}
            }
            let list = rows(data, self);
            self.repair(&list);
            return None;
        }
        let list = rows(data, self);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.step(&list, 1),
            KeyCode::Char('k') | KeyCode::Up => self.step(&list, -1),
            KeyCode::Char('d') if ctrl => self.step(&list, 10),
            KeyCode::Char('u') if ctrl => self.step(&list, -10),
            KeyCode::PageDown => self.step(&list, 10),
            KeyCode::PageUp => self.step(&list, -10),
            KeyCode::Home | KeyCode::Char('g') => self.step(&list, isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.step(&list, isize::MAX),
            KeyCode::Enter | KeyCode::Char('d') if !ctrl => {
                let (repo, pr) = self.selection(data)?;
                let pr_ref = PrRef {
                    repo: repo.repo,
                    number: pr.number,
                };
                let mut detail = Detail::new(pr_ref, pr.clone());
                if key.code == KeyCode::Char('d') {
                    // The diff comes with the focus that asks for it.
                    detail.open_diff(None, None);
                }
                self.detail = Some(detail);
                return Some(PrAction::Opened(pr_ref, pr.updated_at.clone()));
            }
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Char('f') => {
                self.filter = self.filter.next();
                let list = rows(data, self);
                self.repair(&list);
            }
            KeyCode::Char('m') => return Some(PrAction::Repos),
            KeyCode::Char('b') => {
                return self
                    .selection(data)
                    .map(|(_, pr)| PrAction::Browser(pr.url.clone()));
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                let list = rows(data, self);
                self.repair(&list);
            }
            KeyCode::Esc => return Some(PrAction::Close),
            _ => {}
        }
        None
    }

    /// A click on the open pull request: a tab name, a thread to fold, a check, a
    /// file (the selected one opens its diff), or a place in the diff.
    pub fn click(&mut self, x: u16, y: u16, diff: Option<&PrDiff>, layout: &PrLayout) {
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        if let Some(open) = &mut d.diff {
            open.click(x, y, diff, &layout.diff);
            return;
        }
        if y == layout.tab_row
            && let Some((tab, ..)) = layout.tabs.iter().find(|(_, a, b)| (*a..*b).contains(&x))
        {
            d.tab = *tab;
            d.scroll = 0;
            return;
        }
        if !layout.body.contains(ratatui::layout::Position::new(x, y)) {
            return;
        }
        let line = layout.scroll + (y - layout.body.y) as usize;
        match d.tab {
            Tab::Conversation => {
                if let Some(i) = layout.items.iter().rposition(|a| a.line <= line) {
                    d.item = i;
                    // A thread's head folds it.
                    if let Some(id) = layout.items[i]
                        .thread
                        .as_ref()
                        .filter(|_| layout.items[i].line == line)
                        && !d.toggled.remove(id)
                    {
                        d.toggled.insert(id.clone());
                    }
                }
            }
            Tab::Checks if line < layout.checks.len() => d.check = line,
            Tab::Files => {
                if let Some(path) = layout.files.get(line) {
                    if d.file.as_ref() == Some(path) {
                        d.open_diff(Some(path.clone()), diff);
                    } else {
                        d.file = Some(path.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn detail_key(
        &mut self,
        key: KeyEvent,
        diff: Option<&PrDiff>,
        layout: &PrLayout,
    ) -> Option<PrAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (layout.page / 2).max(1);
        let page = layout.page.max(1);
        let d = self.detail.as_mut()?;
        if let Some(view) = &mut d.diff {
            return match view.key(key, d.pr, diff, &layout.diff)? {
                DiffAction::Back(path) => {
                    d.diff = None;
                    d.tab = Tab::Files;
                    if path.is_some() {
                        d.file = path;
                    }
                    None
                }
                DiffAction::Pr(action) => Some(action),
            };
        }
        let checks = d.tab == Tab::Checks;
        let files = d.tab == Tab::Files;
        let talk = d.tab == Tab::Conversation && !layout.items.is_empty();
        let last_item = layout.items.len().saturating_sub(1);
        // The conversation's screen keeps its item's first line on it.
        let show = |d: &mut Detail| {
            if let Some(a) = layout.items.get(d.item) {
                d.scroll = diff::follow(a.line, d.scroll, page).min(layout.end);
            }
        };
        let file_at = d
            .file
            .as_ref()
            .and_then(|f| layout.files.iter().position(|x| x == f));
        match key.code {
            KeyCode::Char('d') if !ctrl => d.open_diff(None, diff),
            KeyCode::Enter if files => {
                let file = d.file.clone().or_else(|| layout.files.first().cloned());
                d.open_diff(file, diff);
            }
            KeyCode::Char('j') | KeyCode::Down if files => {
                let next = file_at.map_or(0, |i| (i + 1).min(layout.files.len().saturating_sub(1)));
                d.file = layout.files.get(next).cloned();
            }
            KeyCode::Char('k') | KeyCode::Up if files => {
                let next = file_at.map_or(0, |i| i.saturating_sub(1));
                d.file = layout.files.get(next).cloned();
            }
            KeyCode::Esc => {
                self.detail = None;
            }
            KeyCode::Tab => {
                d.tab = d.tab.step(1);
                d.scroll = 0;
            }
            KeyCode::BackTab => {
                d.tab = d.tab.step(-1);
                d.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down if checks => {
                d.check = (d.check + 1).min(layout.checks.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up if checks => d.check = d.check.saturating_sub(1),
            KeyCode::Char('j') | KeyCode::Down if talk => {
                d.item = (d.item + 1).min(last_item);
                show(d);
            }
            KeyCode::Char('k') | KeyCode::Up if talk => {
                d.item = d.item.saturating_sub(1);
                show(d);
            }
            KeyCode::Char('j') | KeyCode::Down => d.scroll = (d.scroll + 1).min(layout.end),
            KeyCode::Char('k') | KeyCode::Up => d.scroll = d.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => d.scroll = (d.scroll + half).min(layout.end),
            KeyCode::Char('u') if ctrl => d.scroll = d.scroll.saturating_sub(half),
            KeyCode::PageDown => d.scroll = (d.scroll + page).min(layout.end),
            KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(page),
            KeyCode::Char('n') if talk => {
                if let Some(i) = (d.item + 1..layout.items.len()).find(|i| layout.items[*i].open) {
                    d.item = i;
                    show(d);
                }
            }
            KeyCode::Char('N') if talk => {
                if let Some(i) = (0..d.item).rev().find(|i| layout.items[*i].open) {
                    d.item = i;
                    show(d);
                }
            }
            KeyCode::Enter if talk => {
                if let Some(id) = layout.items.get(d.item).and_then(|a| a.thread.as_ref())
                    && !d.toggled.remove(id)
                {
                    d.toggled.insert(id.clone());
                }
            }
            KeyCode::Char('b') => {
                let check = if checks {
                    layout.checks.get(d.check).cloned().flatten()
                } else {
                    None
                };
                return Some(PrAction::Browser(
                    check.unwrap_or_else(|| d.summary.url.clone()),
                ));
            }
            _ => {}
        }
        None
    }
}

/// The PR view in `area`: the inbox, or the open pull request.
pub fn draw(f: &mut Frame, app: &App, view: &PrView, area: Rect) {
    // Reset first, so a screen that returns early never leaves a stale list rect or
    // thread anchors for the keys and the mouse.
    *app.pr_layout.borrow_mut() = PrLayout::default();
    match &view.detail {
        Some(detail) => match &detail.diff {
            Some(open) => diff::view::draw(f, app, detail.pr, open, area),
            None => detail_view::draw(f, app, detail, area),
        },
        None => inbox_view::draw(f, app, view, area),
    }
}

/// The footer while the PR view is up.
pub fn hint(app: &App, view: &PrView) -> String {
    use crate::keys::{Action, Context};
    let key = |action| app.keymap.key(Context::Grid, action).unwrap_or_default();
    if view.typing {
        return "type to search · ↑/↓ choose · Enter keep · Esc clear".into();
    }
    if let Some(d) = &view.detail {
        if let Some(open) = &d.diff {
            if open.typing {
                return "type to search the paths · ↑/↓ choose · Enter keep · Esc clear".into();
            }
            let other = match app.config.diff.layout {
                termist_core::config::DiffLayout::Unified => "split",
                termist_core::config::DiffLayout::Split => "unified",
            };
            return format!(
                "#{} · Tab panel · J/K file · {{/}} hunk · n/N thread · ^R viewed · s {other} · / search · b browser · Esc back",
                d.pr.number
            );
        }
        if d.tab == Tab::Files {
            return format!(
                "#{} · Tab section · j/k file · Enter diff · b browser · {} refresh · Esc list",
                d.pr.number,
                key(Action::RefreshGitHub)
            );
        }
        return format!(
            "#{} · Tab section · j/k scroll · n/N next open thread · Enter fold · d diff · b browser · {} refresh · Esc list",
            d.pr.number,
            key(Action::RefreshGitHub)
        );
    }
    format!(
        "pull requests · Enter open · / search · f {} · m repos · b browser · {} refresh · Esc grid",
        view.filter.next().label(),
        key(Action::RefreshGitHub)
    )
}

fn fg(color: ratatui::style::Color) -> Style {
    Style::default().fg(color)
}

/// `◇` asked of you, `✓` approved, `✗` changes requested, `·` no verdict.
pub fn review_mark(t: &Theme, pr: &PrSummary) -> Span<'static> {
    if pr.requested_you {
        return Span::styled("◇", fg(t.status(AgentStatus::NeedsFeedback)));
    }
    match pr.decision {
        Some(ReviewDecision::Approved) => Span::styled("✓", fg(t.status(AgentStatus::Unseen))),
        Some(ReviewDecision::ChangesRequested) => Span::styled("✗", t.error),
        _ => Span::styled("·", t.dim),
    }
}

pub fn checks_mark(t: &Theme, checks: Checks) -> Span<'static> {
    match checks {
        Checks::Passing => Span::styled("✓", fg(t.status(AgentStatus::Unseen))),
        Checks::Failing => Span::styled("✗", t.error),
        Checks::Pending => Span::styled("◐", fg(t.status(AgentStatus::Running))),
        Checks::None => Span::raw(" "),
    }
}

pub fn conflict_mark(t: &Theme, pr: &PrSummary) -> Span<'static> {
    match pr.mergeable {
        Mergeable::Conflicting => Span::styled("⚠", t.warn),
        _ => Span::raw(" "),
    }
}

#[cfg(test)]
pub mod fixtures {
    use super::*;
    use termist_core::github::*;

    pub fn summary(number: u32, title: &str, author: &str) -> PrSummary {
        PrSummary {
            number,
            title: title.into(),
            url: format!("https://github.com/acme/site/pull/{number}"),
            author: author.into(),
            draft: false,
            state: PrState::Open,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-02T10:00:00Z".into(),
            head: "feat".into(),
            base: "main".into(),
            additions: 184,
            deletions: 32,
            changed_files: 9,
            mergeable: Mergeable::Yes,
            decision: None,
            requested: vec![],
            requested_you: false,
            verdicts: vec![],
            checks: Checks::Passing,
            unseen: false,
        }
    }

    pub fn repo(id: i64, name: &str, prs: Vec<PrSummary>) -> RepoPrs {
        RepoPrs {
            repo: RepoId(id),
            name: name.into(),
            slug: format!("acme/{name}"),
            state: GhState::Ok,
            viewer: Some("alice".into()),
            total: prs.len() as u32,
            prs,
            fetched_at: Some("2026-10-02T11:59:20Z".into()),
            failed_at: None,
        }
    }

    /// Two threads (one resolved), a comment, an approval and an empty "commented"
    /// review, three checks, two files.
    pub fn detail(summary: PrSummary) -> PrDetail {
        // Alice is you: her comments are yours to edit and delete.
        let c = |author: &str, body: &str, at: &str| Comment {
            id: format!("C-{author}-{at}"),
            author: author.into(),
            body: body.into(),
            created_at: at.into(),
            mine: author == "alice",
            can_edit: author == "alice",
            can_delete: author == "alice",
            pending: false,
        };
        PrDetail {
            summary,
            id: "PR_212".into(),
            head_oid: "h1".into(),
            mine: false,
            pending_review: None,
            body: "Adds a dealer dropdown to the search page.\n\nCloses #198.".into(),
            comments: vec![c("bob", "Screenshots attached ![before](https://x.io/b.png)", "2026-10-02T09:00:00Z")],
            reviews: vec![
                Review {
                    author: "carol".into(),
                    state: ReviewState::Approved,
                    body: "Looks good, one nit below.".into(),
                    submitted_at: "2026-10-02T11:00:00Z".into(),
                },
                Review {
                    author: "carol".into(),
                    state: ReviewState::Commented,
                    body: String::new(),
                    submitted_at: "2026-10-02T10:59:00Z".into(),
                },
            ],
            threads: vec![
                Thread {
                    id: "T1".into(),
                    path: "src/search/DealerFilter.tsx".into(),
                    line: Some(42),
                    side: Side::Right,
                    resolved: false,
                    outdated: false,
                    hunk: "@@ -38,3 +40,5 @@\n  const dealers = useDealers();\n  const [sel, setSel] = useState<string>();\n+ useEffect(() => fetchAll(), []);".into(),
                    comments: vec![
                        c("carol", "This refetches on every mount, can we memoize?", "2026-10-02T10:58:00Z"),
                        c("bob", "Good catch, will fix.", "2026-10-02T11:20:00Z"),
                    ],
                    more: 0,
                    start_line: None,
                    can_reply: true,
                    can_resolve: true,
                },
                Thread {
                    id: "T2".into(),
                    path: "src/api/client.ts".into(),
                    line: Some(10),
                    side: Side::Right,
                    resolved: true,
                    outdated: false,
                    hunk: "@@ -10 +10 @@\n-a\n+b".into(),
                    comments: vec![c("carol", "nit", "2026-10-02T08:00:00Z")],
                    more: 0,
                    start_line: None,
                    can_reply: true,
                    can_resolve: true,
                },
            ],
            checks: vec![
                Check {
                    name: "build".into(),
                    workflow: Some("PR Checks".into()),
                    state: CheckState::Passed,
                    started_at: Some("2026-10-02T09:00:00Z".into()),
                    completed_at: Some("2026-10-02T09:01:12Z".into()),
                    url: Some("https://github.com/acme/site/actions/runs/1".into()),
                },
                Check {
                    name: "e2e".into(),
                    workflow: Some("PR Checks".into()),
                    state: CheckState::Running,
                    started_at: Some("2026-10-02T09:00:00Z".into()),
                    completed_at: None,
                    url: None,
                },
                Check {
                    name: "lint".into(),
                    workflow: None,
                    state: CheckState::Failed,
                    started_at: None,
                    completed_at: None,
                    url: Some("https://github.com/acme/site/actions/runs/2".into()),
                },
            ],
            files: vec![
                FileChange {
                    path: "src/search/DealerFilter.tsx".into(),
                    additions: 120,
                    deletions: 2,
                    change: 'A',
                    viewed: Viewed::Viewed,
                },
                FileChange {
                    path: "src/api/client.ts".into(),
                    additions: 4,
                    deletions: 0,
                    change: 'M',
                    viewed: Viewed::Unviewed,
                },
            ],
            more: More::default(),
        }
    }

    pub fn project_prs(repos: Vec<RepoPrs>) -> ProjectPrs {
        ProjectPrs {
            state: GhState::Ok,
            discovered: repos.len() as u32,
            repos,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};

    fn k(code: K) -> KeyEvent {
        KeyEvent::new(code, M::NONE)
    }

    fn data() -> ProjectPrs {
        let mut wip = summary(201, "WIP: new header", "alice");
        wip.draft = true;
        let mut review = summary(212, "Add a dealer filter", "bob");
        review.requested_you = true;
        project_prs(vec![
            repo(
                1,
                "site",
                vec![wip, review, summary(209, "Fix lazy images", "alice")],
            ),
            repo(2, "admin", vec![]),
        ])
    }

    fn numbers(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Repo { repo, shown } => format!("{}:{shown}", repo.name),
                Row::Pr { pr, .. } => pr.number.to_string(),
                Row::Nothing { .. } => "-".into(),
                Row::More { more, .. } => format!("+{more}"),
            })
            .collect()
    }

    #[test]
    fn repos_head_their_prs_and_drafts_come_last() {
        let d = data();
        assert_eq!(
            numbers(&rows(&d, &PrView::default())),
            ["site:3", "212", "209", "201", "admin:0", "-"]
        );
    }

    #[test]
    fn filters_and_the_search_narrow_the_list() {
        let d = data();
        let mut v = PrView {
            filter: Filter::ToReview,
            ..PrView::default()
        };
        assert_eq!(numbers(&rows(&d, &v)), ["site:1", "212", "admin:0", "-"]);
        v.filter = Filter::Mine;
        assert_eq!(
            numbers(&rows(&d, &v)),
            ["site:2", "209", "201", "admin:0", "-"]
        );
        v.filter = Filter::All;
        v.query = "lazy".into();
        assert_eq!(numbers(&rows(&d, &v)), ["site:1", "209", "admin:0", "-"]);
        v.query = "212".into();
        assert_eq!(numbers(&rows(&d, &v))[1], "212");
    }

    #[test]
    fn more_than_was_read_is_counted() {
        let mut d = data();
        d.repos[0].total = 80;
        assert_eq!(numbers(&rows(&d, &PrView::default()))[4], "+77");
    }

    #[test]
    fn a_pr_that_goes_moves_the_selection_to_the_next() {
        let mut d = data();
        let mut v = PrView::default();
        v.repair(&rows(&d, &v));
        assert_eq!(v.selected.map(|p| p.number), Some(212));
        d.repos[0].prs.retain(|p| p.number != 212);
        v.repair(&rows(&d, &v));
        assert_eq!(
            v.selected.map(|p| p.number),
            Some(209),
            "the one now in its place"
        );
        v.key(k(K::Char('j')), &d, None, &PrLayout::default());
        assert_eq!(v.selected.map(|p| p.number), Some(201));
        d.repos[0].prs.retain(|p| p.number != 201);
        v.repair(&rows(&d, &v));
        assert_eq!(
            v.selected.map(|p| p.number),
            Some(209),
            "the last one goes: the one above"
        );
    }

    #[test]
    fn enter_opens_and_esc_goes_back() {
        let d = data();
        let mut v = PrView::default();
        v.repair(&rows(&d, &v));
        let layout = PrLayout::default();
        let opened = v.key(k(K::Enter), &d, None, &layout);
        let pr = PrRef {
            repo: termist_core::github::RepoId(1),
            number: 212,
        };
        assert_eq!(
            opened,
            Some(PrAction::Opened(pr, "2026-10-02T10:00:00Z".into()))
        );
        assert_eq!(v.detail.as_ref().map(|d| d.pr), Some(pr));
        v.key(k(K::Tab), &d, None, &layout);
        assert_eq!(v.detail.as_ref().unwrap().tab, Tab::Conversation);
        assert_eq!(v.key(k(K::Esc), &d, None, &layout), None);
        assert!(v.detail.is_none());
        assert_eq!(v.key(k(K::Esc), &d, None, &layout), Some(PrAction::Close));
    }

    #[test]
    fn typing_searches_and_esc_clears() {
        let d = data();
        let mut v = PrView::default();
        v.key(k(K::Char('/')), &d, None, &PrLayout::default());
        for c in "lazy".chars() {
            v.key(k(K::Char(c)), &d, None, &PrLayout::default());
        }
        assert_eq!(v.query, "lazy");
        assert_eq!(v.selected.map(|p| p.number), Some(209));
        v.key(k(K::Esc), &d, None, &PrLayout::default());
        assert!(!v.typing && v.query.is_empty());
    }

    #[test]
    fn the_conversation_moves_item_by_item_and_n_stops_at_open_threads() {
        let d = data();
        let mut v = PrView::default();
        v.repair(&rows(&d, &v));
        v.key(k(K::Enter), &d, None, &PrLayout::default());
        v.detail.as_mut().unwrap().tab = Tab::Conversation;
        let thread = |line, id: &str, open| Item {
            line,
            thread: Some(id.into()),
            open,
            ..Item::default()
        };
        let layout = PrLayout {
            end: 100,
            page: 10,
            items: vec![
                Item::default(),
                thread(4, "T1", true),
                thread(12, "T2", false),
                thread(20, "T3", true),
            ],
            ..PrLayout::default()
        };
        let item = |v: &PrView| v.detail.as_ref().unwrap().item;
        v.key(k(K::Char('j')), &d, None, &layout);
        assert_eq!(item(&v), 1);
        v.key(k(K::Char('n')), &d, None, &layout);
        assert_eq!(item(&v), 3, "a resolved thread is passed");
        assert_eq!(v.detail.as_ref().unwrap().scroll, 11, "its line on screen");
        v.key(k(K::Char('N')), &d, None, &layout);
        assert_eq!(item(&v), 1);
        v.key(k(K::Enter), &d, None, &layout);
        assert!(v.detail.as_ref().unwrap().toggled.contains("T1"));
        v.key(k(K::Enter), &d, None, &layout);
        assert!(v.detail.as_ref().unwrap().toggled.is_empty());
        for _ in 0..9 {
            v.key(k(K::Char('j')), &d, None, &layout);
        }
        assert_eq!(item(&v), 3, "held at the last");
        let b = v.key(k(K::Char('b')), &d, None, &layout);
        assert_eq!(
            b,
            Some(PrAction::Browser(
                "https://github.com/acme/site/pull/212".into()
            ))
        );
    }
}
