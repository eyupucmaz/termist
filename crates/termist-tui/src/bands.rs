//! The grid's bands: a project's cards grouped by the worktree they run in, and the rows
//! they are drawn in.
use crate::ui::CARD_H;
use std::path::{Path, PathBuf};
use termist_core::{Place, SessionId, SessionInfo, WorktreeInfo};

/// The cards that run in one worktree (or one folder outside any repo).
#[derive(Debug)]
pub struct Band<'a> {
    pub root: PathBuf,
    /// The first card's place, once the daemon read it.
    pub place: Option<&'a Place>,
    pub cards: Vec<&'a SessionInfo>,
    /// The worktree as the daemon keeps it, when it is one: its summary, its end.
    pub worktree: Option<&'a WorktreeInfo>,
}

/// Where a card's band is: its worktree, or its folder until that is known.
fn root_of(s: &SessionInfo) -> &Path {
    s.place
        .as_deref()
        .map(|p| p.root.as_path())
        .unwrap_or(&s.cwd)
}

/// The project's cards by worktree: the project folder's band first, then the others
/// in the order their first cards came.
pub fn bands<'a>(
    project: &Path,
    sessions: &[&'a SessionInfo],
    worktrees: &'a [WorktreeInfo],
) -> Vec<Band<'a>> {
    let mut bands: Vec<Band<'a>> = vec![];
    for s in sessions {
        let root = root_of(s);
        match bands.iter_mut().find(|b| b.root == root) {
            Some(b) => {
                b.place = b.place.or(s.place.as_deref());
                b.cards.push(s);
            }
            None => bands.push(Band {
                root: root.to_path_buf(),
                place: s.place.as_deref(),
                cards: vec![s],
                worktree: None,
            }),
        }
    }
    for band in &mut bands {
        band.worktree = worktrees.iter().find(|w| w.path == band.root);
    }
    // Worktrees shown with no card in them: a band each, after the others.
    for w in worktrees.iter().filter(|w| w.shown) {
        if !bands.iter().any(|b| b.root == w.path) {
            bands.push(Band {
                root: w.path.clone(),
                place: None,
                cards: vec![],
                worktree: Some(w),
            });
        }
    }
    // The band holding the project's folder (its repo, or the folder itself) leads.
    if let Some(i) = bands.iter().position(|b| project.starts_with(&b.root)) {
        let home = bands.remove(i);
        bands.insert(0, home);
    }
    bands
}

/// Band headers are drawn when there is more than one band, or the one band has a
/// pull request: a project without worktrees looks as it always did.
pub fn headers(bands: &[Band]) -> bool {
    bands.len() > 1
        || bands
            .iter()
            .any(|b| b.place.is_some_and(|p| p.pr.is_some()))
}

/// A place in the grid that can be selected: a card, or the stand-in of a band with
/// no cards (its worktree's folder).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Card(SessionId),
    Empty(PathBuf),
}

/// A row of the grid: maybe a band's header line, then up to a row of cards (or the
/// stand-in of a band with none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The band whose header is drawn above the cards: its index.
    pub header: Option<usize>,
    pub slots: Vec<Slot>,
}

impl Row {
    pub fn height(&self) -> u16 {
        CARD_H + u16::from(self.header.is_some())
    }
}

/// The rows the bands take with `per_row` cards to a row: each band starts a row.
pub fn rows(bands: &[Band], per_row: usize, headers: bool) -> Vec<Row> {
    let per_row = per_row.max(1);
    let mut rows = vec![];
    for (i, band) in bands.iter().enumerate() {
        if band.cards.is_empty() {
            rows.push(Row {
                header: headers.then_some(i),
                slots: vec![Slot::Empty(band.root.clone())],
            });
            continue;
        }
        for (n, chunk) in band.cards.chunks(per_row).enumerate() {
            rows.push(Row {
                header: (headers && n == 0).then_some(i),
                slots: chunk.iter().map(|s| Slot::Card(s.id)).collect(),
            });
        }
    }
    rows
}

/// The lines the cards take: for each band its rows of cards, and its header.
pub fn height(counts: &[usize], per_row: usize, headers: bool) -> u16 {
    counts
        .iter()
        .map(|n| n.div_ceil(per_row.max(1)) as u16 * CARD_H + u16::from(headers))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::github::{PrRef, RepoId};
    use termist_core::{AgentStatus, ProjectId, SessionKind};

    fn card(name: &str, cwd: &str, place: Option<(&str, Option<u32>)>) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project: ProjectId::new(),
            kind: SessionKind::Shell,
            name: name.into(),
            status: AgentStatus::Fresh,
            agent_session_id: None,
            title: None,
            last_activity_ms: 0,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: cwd.into(),
            place: place.map(|(root, pr)| {
                Box::new(Place {
                    root: root.into(),
                    branch: Some("b".into()),
                    commit: None,
                    repo: Some(RepoId(1)),
                    pr: pr.map(|number| PrRef {
                        repo: RepoId(1),
                        number,
                    }),
                    gone: false,
                })
            }),
        }
    }

    #[test]
    fn cards_group_by_worktree_and_the_project_s_own_band_leads() {
        let a = card(
            "a",
            "/w/site-worktrees/fix",
            Some(("/w/site-worktrees/fix", Some(212))),
        );
        let b = card("b", "/w/site/src", Some(("/w/site", None)));
        let c = card(
            "c",
            "/w/site-worktrees/fix/src",
            Some(("/w/site-worktrees/fix", Some(212))),
        );
        let d = card("d", "/w/site", None); // not read yet: its folder
        let all = [&a, &b, &c, &d];
        let bands = bands(Path::new("/w/site"), &all, &[]);
        let names: Vec<Vec<&str>> = bands
            .iter()
            .map(|b| b.cards.iter().map(|s| s.name.as_str()).collect())
            .collect();
        assert_eq!(names, [vec!["b", "d"], vec!["a", "c"]]);
        assert!(headers(&bands));
    }

    #[test]
    fn one_band_without_a_pull_request_has_no_header() {
        let a = card("a", "/w/site", Some(("/w/site", None)));
        let b = card("b", "/w/site", None);
        let bands = bands(Path::new("/w/site"), &[&a, &b], &[]);
        assert_eq!(bands.len(), 1);
        assert!(!headers(&bands));
        let a = card("a", "/w/site", Some(("/w/site", Some(212))));
        assert!(headers(&super::bands(Path::new("/w/site"), &[&a], &[])));
    }

    #[test]
    fn each_band_starts_a_row_under_its_header() {
        let cards: Vec<SessionInfo> = (0..5)
            .map(|i| match i {
                0..3 => card("m", "/w/site", None),
                _ => card("f", "/w/fix", None),
            })
            .collect();
        let all: Vec<&SessionInfo> = cards.iter().collect();
        let bands = bands(Path::new("/w/site"), &all, &[]);
        let rows = rows(&bands, 2, true);
        let shape: Vec<(Option<usize>, usize)> =
            rows.iter().map(|r| (r.header, r.slots.len())).collect();
        assert_eq!(shape, [(Some(0), 2), (None, 1), (Some(1), 2)]);
        assert_eq!(rows[0].height(), CARD_H + 1);
        assert_eq!(height(&[3, 2], 2, true), 3 * CARD_H + 2);
        assert_eq!(height(&[3, 2], 2, false), 3 * CARD_H);
    }
}
