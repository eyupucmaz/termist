//! The inbox: a project's open pull requests repo by repo, and on a wide screen the
//! selected one beside them.
use super::markdown::{cut, render, width_of};
use super::{Filter, PrView, ProjectPrs, Row, checks_mark, conflict_mark, review_mark, rows};
use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use termist_core::AgentStatus;
use termist_core::github::{
    Checks, GhState, Mergeable, PrRef, PrSummary, ReviewDecision, ReviewState, age, unix_secs,
};

/// From this wide the selected pull request shows beside the list.
pub const PREVIEW_FROM: u16 = 100;

/// What to tell the user for a project-wide or repo-wide trouble.
pub fn trouble(state: &GhState) -> Option<Vec<String>> {
    Some(match state {
        GhState::Ok => return None,
        GhState::NoGh => vec![
            "termist reads pull requests through the GitHub CLI.".into(),
            "Install gh, then run: gh auth login".into(),
        ],
        GhState::LoggedOut => vec!["gh is not logged in.".into(), "Run: gh auth login".into()],
        GhState::NoAccess => vec!["No logged-in account can see this repo.".into()],
        GhState::RateLimited { .. } => {
            vec!["GitHub's hourly limit is used up; termist reads again later.".into()]
        }
        GhState::Failed(why) => vec![format!("Could not read GitHub: {why}")],
    })
}

/// A few words for a repo heading.
pub fn short_trouble(state: &GhState) -> Option<&'static str> {
    match state {
        GhState::Ok => None,
        GhState::NoGh => Some("no gh"),
        GhState::LoggedOut => Some("logged out"),
        GhState::NoAccess => Some("no access"),
        GhState::RateLimited { .. } => Some("rate limited"),
        GhState::Failed(_) => Some("failed"),
    }
}

fn say(f: &mut Frame, t: &Theme, area: Rect, lines: &[String]) {
    let lines: Vec<Line> = lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), t.dim)))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" pull requests ");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

pub fn draw(f: &mut Frame, app: &App, view: &PrView, area: Rect) {
    let t = &app.theme;
    let Some(project) = app.project else {
        return say(f, t, area, &["No project open.".into()]);
    };
    if !app.config.github.enabled {
        return say(
            f,
            t,
            area,
            &[
                "Pull requests are off.".into(),
                "Turn them on in the settings (s), or with github.enabled in config.toml.".into(),
            ],
        );
    }
    let Some(data) = app.prs.get(&project) else {
        return say(f, t, area, &["Reading GitHub…".into()]);
    };
    if let Some(lines) = trouble(&data.state) {
        return say(f, t, area, &lines);
    }
    if data.discovered == 0 {
        return say(
            f,
            t,
            area,
            &[
                "This project has no GitHub repo.".into(),
                "termist looks at the folder's own remote, or at the repos one level below it."
                    .into(),
            ],
        );
    }
    let name = app
        .state
        .projects
        .iter()
        .find(|p| p.id == project)
        .map_or("", |p| p.name.as_str());
    if area.width >= PREVIEW_FROM {
        let left = Rect {
            width: area.width / 2,
            ..area
        };
        let right = Rect {
            x: area.x + left.width,
            width: area.width - left.width,
            ..area
        };
        draw_list(f, app, view, data, name, left);
        draw_preview(f, app, view, data, right);
    } else {
        draw_list(f, app, view, data, name, area);
    }
}

fn draw_list(f: &mut Frame, app: &App, view: &PrView, data: &ProjectPrs, name: &str, area: Rect) {
    let t = &app.theme;
    let now = app.now_secs();
    let fetched = data
        .repos
        .iter()
        .filter_map(|r| r.fetched_at.as_deref())
        .filter_map(unix_secs)
        .max();
    let failed = data
        .repos
        .iter()
        .filter_map(|r| r.failed_at.as_deref())
        .filter_map(unix_secs)
        .max();
    let mut title = vec![Span::raw(format!(
        " {name} · {} of {} repos ",
        data.repos.len(),
        data.discovered
    ))];
    title.push(match (fetched, failed) {
        (_, Some(bad)) if fetched.is_none_or(|ok| bad > ok) => {
            Span::styled(format!("⟳ failed {} ago ", age(now - bad)), t.warn)
        }
        (Some(ok), _) => Span::styled(format!("⟳ {} ago ", age(now - ok)), t.dim),
        _ => Span::styled("⟳ reading… ", t.dim),
    });
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let mut chips = vec![Span::raw(" ")];
    for filter in Filter::ALL {
        let style = if filter == view.filter {
            t.accent.add_modifier(Modifier::BOLD)
        } else {
            t.dim
        };
        chips.push(Span::styled(filter.label(), style));
        chips.push(Span::raw("  "));
    }
    if view.typing || !view.query.is_empty() {
        let cursor = if view.typing { "▏" } else { "" };
        chips.push(Span::styled(format!("/{}{cursor}", view.query), t.focus));
    }
    f.render_widget(
        Paragraph::new(Line::from(chips)),
        Rect { height: 1, ..inner },
    );
    let list = Rect {
        y: inner.y + 2.min(inner.height),
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let all = rows(data, view);
    let selected = all
        .iter()
        .position(|r| r.pr_ref().is_some() && r.pr_ref() == view.selected);
    let h = list.height as usize;
    let first = match selected {
        Some(i) if h > 0 && i >= h => i + 1 - h,
        _ => 0,
    };
    let lines: Vec<Line> = all
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, row)| row_line(t, row, Some(i) == selected, list.width as usize, now))
        .collect();
    f.render_widget(Paragraph::new(lines), list);
    let mut layout = app.pr_layout.borrow_mut();
    layout.list = list;
    layout.first = first;
}

fn row_line(t: &Theme, row: &Row, selected: bool, width: usize, now: i64) -> Line<'static> {
    match row {
        Row::Repo { repo, shown } => {
            let mut spans = vec![Span::styled(
                format!(" {}", repo.name),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if let Some(why) = short_trouble(&repo.state) {
                spans.push(Span::styled(format!(" · {why}"), t.warn));
            }
            let used: usize = spans.iter().map(Span::width).sum();
            let count = format!("{shown} ");
            spans.push(Span::raw(
                " ".repeat(width.saturating_sub(used + count.len())),
            ));
            spans.push(Span::styled(count, t.dim));
            Line::from(spans)
        }
        Row::Nothing { repo } => {
            // Never read is not an answer yet.
            let what = match (&repo.fetched_at, &repo.state) {
                (None, GhState::Ok) => "   reading…",
                (None, _) => "   not read yet",
                _ if repo.prs.is_empty() => "   no open pull requests",
                _ => "   nothing matches",
            };
            Line::from(Span::styled(what, t.dim))
        }
        Row::More { more, .. } => {
            Line::from(Span::styled(format!("   +{more} more on GitHub"), t.dim))
        }
        Row::Pr { pr, .. } => pr_line(t, pr, selected, width, now),
    }
}

fn pr_line(t: &Theme, pr: &PrSummary, selected: bool, width: usize, now: i64) -> Line<'static> {
    let base = if pr.draft { t.dim } else { Style::default() };
    let marker = if selected {
        Span::styled("▌", t.accent)
    } else {
        Span::raw(" ")
    };
    let dot = if pr.unseen {
        Span::styled("•", t.accent)
    } else {
        Span::raw(" ")
    };
    let number = Span::styled(format!(" #{:<4} ", pr.number), t.dim);
    let marks: Vec<Span> = if pr.draft {
        vec![Span::styled("draft", t.dim)]
    } else {
        vec![
            review_mark(t, pr),
            Span::raw(" "),
            checks_mark(t, pr.checks),
            Span::raw(" "),
            conflict_mark(t, pr),
        ]
    };
    let when = unix_secs(&pr.updated_at)
        .map(|u| age(now - u))
        .unwrap_or_default();
    let when = Span::styled(format!(" {when:>3} "), t.dim);
    let fixed = 2 + number.width() + 1 + 5 + when.width();
    let room = width.saturating_sub(fixed);
    let title = cut(&pr.title, room);
    let pad = room.saturating_sub(width_of(&title));
    let mut spans = vec![marker, dot, number, Span::styled(title, base)];
    spans.push(Span::raw(" ".repeat(pad + 1)));
    spans.extend(marks);
    spans.push(when);
    if selected {
        for s in &mut spans {
            s.style = s.style.patch(t.selection);
        }
    }
    Line::from(spans)
}

fn draw_preview(f: &mut Frame, app: &App, view: &PrView, data: &ProjectPrs, area: Rect) {
    let t = &app.theme;
    let Some((repo, pr)) = view.selection(data) else {
        f.render_widget(Block::default().borders(Borders::ALL), area);
        return;
    };
    let w = area.width.saturating_sub(2) as usize;
    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} ",
        cut(&format!("#{} {}", pr.number, pr.title), w.saturating_sub(2))
    ));
    let mut lines = vec![
        Line::from(Span::styled(
            cut(
                &format!("{} · {} · {} → {}", repo.slug, pr.author, pr.head, pr.base),
                w,
            ),
            t.dim,
        )),
        status_line(t, pr),
        Line::default(),
    ];
    let mut reviews: Vec<Line> = pr
        .requested
        .iter()
        .map(|who| {
            let who = if pr.requested_you && repo.viewer.as_deref() == Some(who) {
                "you"
            } else {
                who.as_str()
            };
            Line::from(vec![
                review_mark(t, pr),
                Span::raw(format!(" {who:<14} ")),
                Span::styled("requested", t.dim),
            ])
        })
        .collect();
    reviews.extend(pr.verdicts.iter().map(|(who, state)| {
        let (mark, word) = match state {
            ReviewState::Approved => (
                Span::styled("✓", Style::default().fg(t.status(AgentStatus::Unseen))),
                "approved",
            ),
            _ => (Span::styled("✗", t.error), "changes requested"),
        };
        Line::from(vec![
            mark,
            Span::raw(format!(" {who:<14} ")),
            Span::styled(word, t.dim),
        ])
    }));
    if !reviews.is_empty() {
        lines.push(Line::from(Span::styled(
            "Reviews",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.extend(reviews);
        lines.push(Line::default());
    }
    let pr_ref = PrRef {
        repo: repo.repo,
        number: pr.number,
    };
    match app.pr_details.get(&pr_ref).and_then(|(_, d)| d.as_ref()) {
        Some(detail) if !detail.body.trim().is_empty() => {
            lines.extend(render(&detail.body, w as u16, t));
        }
        _ => lines.push(Line::from(Span::styled(
            "Enter: the whole pull request",
            t.dim,
        ))),
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// `◇ your review · ✓ checks · ⚠ conflicts · +184 −32 · 9 files`.
pub fn status_line(t: &Theme, pr: &PrSummary) -> Line<'static> {
    fn part(spans: &mut Vec<Span<'static>>, t: &Theme, mark: Span<'static>, words: &str) {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", t.dim));
        }
        spans.push(mark);
        spans.push(Span::raw(format!(" {words}")));
    }
    let mut spans = Vec::new();
    if pr.requested_you {
        part(&mut spans, t, review_mark(t, pr), "your review");
    } else {
        match pr.decision {
            Some(ReviewDecision::Approved) => part(&mut spans, t, review_mark(t, pr), "approved"),
            Some(ReviewDecision::ChangesRequested) => {
                part(&mut spans, t, review_mark(t, pr), "changes requested")
            }
            _ => {}
        }
    }
    match pr.checks {
        Checks::Passing => part(&mut spans, t, checks_mark(t, pr.checks), "checks"),
        Checks::Failing => part(&mut spans, t, checks_mark(t, pr.checks), "checks failing"),
        Checks::Pending => part(&mut spans, t, checks_mark(t, pr.checks), "checks running"),
        Checks::None => {}
    }
    if pr.mergeable == Mergeable::Conflicting {
        part(&mut spans, t, conflict_mark(t, pr), "conflicts");
    }
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", t.dim));
    }
    spans.push(Span::styled(
        format!(
            "+{} −{} · {} files",
            pr.additions, pr.deletions, pr.changed_files
        ),
        t.dim,
    ));
    Line::from(spans)
}
