//! Defter: the Issues tab of the pull request view. A project's open issues repo by
//! repo, as the daemon read them while the tab is open.
use crate::list_picker::matches;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termist_core::github::{GhState, IssueSummary, RepoId, RepoIssues};

/// A project's issues as the daemon last sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIssues {
    pub state: GhState,
    pub repos: Vec<RepoIssues>,
}

impl Default for ProjectIssues {
    fn default() -> Self {
        ProjectIssues {
            state: GhState::Ok,
            repos: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IssueFilter {
    #[default]
    All,
    /// Assigned to you, by login.
    Assigned,
    /// Opened by you.
    Mine,
}

impl IssueFilter {
    pub const ALL: [IssueFilter; 3] = [IssueFilter::All, IssueFilter::Assigned, IssueFilter::Mine];

    pub fn next(self) -> IssueFilter {
        match self {
            IssueFilter::All => IssueFilter::Assigned,
            IssueFilter::Assigned => IssueFilter::Mine,
            IssueFilter::Mine => IssueFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            IssueFilter::All => "all",
            IssueFilter::Assigned => "assigned to you",
            IssueFilter::Mine => "mine",
        }
    }
}

/// An issue by repo and number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IssueRef {
    pub repo: RepoId,
    pub number: u32,
}

/// The Issues tab: its filter, search and selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueList {
    pub filter: IssueFilter,
    /// The `/` search.
    pub query: String,
    /// Keys go into `query`.
    pub typing: bool,
    pub selected: Option<IssueRef>,
    /// The selection's place among the issues: where it lands when its issue goes.
    pub index: usize,
}

/// A line of the issue list.
#[derive(Clone, Copy, Debug)]
pub enum IssueRow<'a> {
    Repo {
        repo: &'a RepoIssues,
        shown: usize,
    },
    Issue {
        repo: &'a RepoIssues,
        issue: &'a IssueSummary,
    },
    /// A repo with nothing to show.
    Nothing {
        repo: &'a RepoIssues,
    },
    /// Open issues beyond those read.
    More {
        repo: &'a RepoIssues,
        more: u32,
    },
}

impl IssueRow<'_> {
    pub fn issue_ref(&self) -> Option<IssueRef> {
        match self {
            IssueRow::Issue { repo, issue } => Some(IssueRef {
                repo: repo.repo,
                number: issue.number,
            }),
            _ => None,
        }
    }
}

fn keeps(list: &IssueList, repo: &RepoIssues, issue: &IssueSummary) -> bool {
    let filter = match list.filter {
        IssueFilter::All => true,
        IssueFilter::Assigned => issue.assigned_you,
        IssueFilter::Mine => repo.viewer.as_deref() == Some(issue.author.as_str()),
    };
    filter
        && (list.query.is_empty()
            || matches(
                &list.query,
                &format!(
                    "#{} {} {}",
                    issue.number,
                    issue.title,
                    issue.labels.join(" ")
                ),
            ))
}

/// Each repo's heading, then its issues as GitHub sorted them: last updated first. A
/// repo with issues turned off has its heading alone.
pub fn rows<'a>(data: &'a ProjectIssues, list: &IssueList) -> Vec<IssueRow<'a>> {
    let mut out = Vec::new();
    for repo in &data.repos {
        let issues: Vec<&IssueSummary> = repo
            .issues
            .iter()
            .filter(|i| keeps(list, repo, i))
            .collect();
        out.push(IssueRow::Repo {
            repo,
            shown: issues.len(),
        });
        if !repo.enabled {
            continue;
        }
        if issues.is_empty() {
            out.push(IssueRow::Nothing { repo });
        }
        out.extend(
            issues
                .into_iter()
                .map(|issue| IssueRow::Issue { repo, issue }),
        );
        let more = repo.total.saturating_sub(repo.issues.len() as u32);
        if more > 0 && list.filter == IssueFilter::All && list.query.is_empty() {
            out.push(IssueRow::More { repo, more });
        }
    }
    out
}

/// What a key in the Issues tab asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueAction {
    /// Back to the grid.
    Close,
    Browser(String),
    Repos,
}

impl IssueList {
    pub fn repair(&mut self, rows: &[IssueRow]) {
        let issues: Vec<IssueRef> = rows.iter().filter_map(IssueRow::issue_ref).collect();
        if let Some(i) = self
            .selected
            .and_then(|s| issues.iter().position(|r| *r == s))
        {
            self.index = i;
            return;
        }
        self.index = self.index.min(issues.len().saturating_sub(1));
        self.selected = issues.get(self.index).copied();
    }

    fn step(&mut self, rows: &[IssueRow], delta: isize) {
        let issues: Vec<IssueRef> = rows.iter().filter_map(IssueRow::issue_ref).collect();
        if issues.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| issues.iter().position(|r| *r == s))
            .unwrap_or(0) as isize;
        self.index = at.saturating_add(delta).clamp(0, issues.len() as isize - 1) as usize;
        self.selected = Some(issues[self.index]);
    }

    pub fn selection<'a>(
        &self,
        data: &'a ProjectIssues,
    ) -> Option<(&'a RepoIssues, &'a IssueSummary)> {
        let s = self.selected?;
        let repo = data.repos.iter().find(|r| r.repo == s.repo)?;
        Some((repo, repo.issues.iter().find(|i| i.number == s.number)?))
    }

    pub fn key(&mut self, key: KeyEvent, data: &ProjectIssues) -> Option<IssueAction> {
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
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Char('f') => {
                self.filter = self.filter.next();
                let list = rows(data, self);
                self.repair(&list);
            }
            KeyCode::Char('m') => return Some(IssueAction::Repos),
            KeyCode::Char('b') => {
                return self
                    .selection(data)
                    .map(|(_, i)| IssueAction::Browser(i.url.clone()));
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                let list = rows(data, self);
                self.repair(&list);
            }
            KeyCode::Esc => return Some(IssueAction::Close),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn issue(number: u32, title: &str) -> IssueSummary {
        IssueSummary {
            number,
            title: title.into(),
            url: format!("https://github.com/acme/site/issues/{number}"),
            body: "It breaks.".into(),
            author: "bob".into(),
            labels: vec!["bug".into()],
            assignees: vec![],
            assigned_you: false,
            comments: 0,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-02T10:00:00Z".into(),
        }
    }

    pub fn repo(id: i64, name: &str, issues: Vec<IssueSummary>) -> RepoIssues {
        RepoIssues {
            repo: RepoId(id),
            name: name.into(),
            slug: format!("acme/{name}"),
            state: GhState::Ok,
            viewer: Some("alice".into()),
            enabled: true,
            total: issues.len() as u32,
            issues,
            fetched_at: Some("2026-10-02T10:00:05Z".into()),
            failed_at: None,
        }
    }

    /// site: #123 assigned to you, #7 yours; api: issues turned off.
    pub fn data() -> ProjectIssues {
        let mut assigned = issue(123, "Login redirect loses the query");
        assigned.assigned_you = true;
        assigned.labels = vec!["bug".into(), "ui".into()];
        let mut mine = issue(7, "Docs for the dealer filter");
        mine.author = "alice".into();
        mine.labels = vec!["docs".into()];
        let mut site = repo(1, "site", vec![assigned, mine]);
        site.total = 5;
        let mut api = repo(2, "api", vec![]);
        api.enabled = false;
        ProjectIssues {
            state: GhState::Ok,
            repos: vec![site, api],
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn numbers(data: &ProjectIssues, list: &IssueList) -> Vec<u32> {
        rows(data, list)
            .iter()
            .filter_map(|r| r.issue_ref().map(|i| i.number))
            .collect()
    }

    #[test]
    fn the_list_is_repo_by_repo_and_a_repo_without_issues_has_its_heading_alone() {
        let data = data();
        let list = IssueList::default();
        let shape: Vec<String> = rows(&data, &list)
            .iter()
            .map(|r| match r {
                IssueRow::Repo { repo, shown } => format!("{} {shown}", repo.name),
                IssueRow::Issue { issue, .. } => format!("#{}", issue.number),
                IssueRow::Nothing { .. } => "nothing".into(),
                IssueRow::More { more, .. } => format!("+{more}"),
            })
            .collect();
        assert_eq!(shape, ["site 2", "#123", "#7", "+3", "api 0"]);
    }

    #[test]
    fn f_keeps_yours_and_slash_searches_titles_numbers_and_labels() {
        let data = data();
        let mut list = IssueList::default();
        list.key(key(KeyCode::Char('f')), &data);
        assert_eq!(list.filter, IssueFilter::Assigned);
        assert_eq!(numbers(&data, &list), [123]);
        list.key(key(KeyCode::Char('f')), &data);
        assert_eq!(numbers(&data, &list), [7], "opened by you");
        list.key(key(KeyCode::Char('f')), &data);
        list.key(key(KeyCode::Char('/')), &data);
        for c in "docs".chars() {
            list.key(key(KeyCode::Char(c)), &data);
        }
        assert_eq!(numbers(&data, &list), [7], "a label");
        assert_eq!(list.selected.map(|s| s.number), Some(7));
        list.key(key(KeyCode::Esc), &data);
        list.key(key(KeyCode::Char('/')), &data);
        list.key(key(KeyCode::Char('1')), &data);
        assert_eq!(numbers(&data, &list), [123], "a number");
    }

    #[test]
    fn keys_move_open_the_browser_and_leave() {
        let data = data();
        let mut list = IssueList::default();
        list.repair(&rows(&data, &list));
        assert_eq!(list.selected.map(|s| s.number), Some(123));
        list.key(key(KeyCode::Char('j')), &data);
        assert_eq!(
            list.key(key(KeyCode::Char('b')), &data),
            Some(IssueAction::Browser(
                "https://github.com/acme/site/issues/7".into()
            ))
        );
        assert_eq!(
            list.key(key(KeyCode::Char('m')), &data),
            Some(IssueAction::Repos)
        );
        assert_eq!(list.key(key(KeyCode::Esc), &data), Some(IssueAction::Close));
    }
}
