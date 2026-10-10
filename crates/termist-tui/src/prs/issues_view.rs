//! The Issues tab: a project's open issues repo by repo, and on a wide screen the
//! selected one beside them.
use super::inbox_view::{PREVIEW_FROM, short_trouble, trouble};
use super::issues::{IssueFilter, IssueList, IssueRow, ProjectIssues, rows};
use super::markdown::{cut, render, strip_comments, width_of};
use super::{PrView, section_title};
use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use termist_core::github::{GhState, IssueSummary, RepoIssues, age, unix_secs};

fn say(f: &mut Frame, t: &Theme, area: Rect, lines: &[String]) {
    let lines: Vec<Line> = lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), t.dim)))
        .collect();
    let block = Block::default().borders(Borders::ALL).title(" issues ");
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
                "Issues are off.".into(),
                "Turn them on in the settings (s), or with github.enabled in config.toml.".into(),
            ],
        );
    }
    let Some(data) = app.issues.get(&project) else {
        return say(f, t, area, &["Reading GitHub…".into()]);
    };
    if let Some(lines) = trouble(&data.state, "issues") {
        return say(f, t, area, &lines);
    }
    if data.repos.is_empty() {
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
    if view.issues.reading.is_some()
        && let Some((repo, issue)) = view.issues.selection(data)
    {
        return draw_reading(f, app, view, repo, issue, area);
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
        draw_preview(f, app, &view.issues, data, right);
    } else {
        draw_list(f, app, view, data, name, area);
    }
}

fn draw_list(
    f: &mut Frame,
    app: &App,
    view: &PrView,
    data: &ProjectIssues,
    name: &str,
    area: Rect,
) {
    let t = &app.theme;
    let list_state = &view.issues;
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
    let mut title = section_title(app, view, area);
    title.push(Span::raw(format!("· {name} ")));
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
    for filter in IssueFilter::ALL {
        let style = if filter == list_state.filter {
            t.accent.add_modifier(Modifier::BOLD)
        } else {
            t.dim
        };
        chips.push(Span::styled(filter.label(), style));
        chips.push(Span::raw("  "));
    }
    if list_state.typing || !list_state.query.is_empty() {
        let cursor = if list_state.typing { "▏" } else { "" };
        chips.push(Span::styled(
            format!("/{}{cursor}", list_state.query),
            t.focus,
        ));
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
    let all = rows(data, list_state);
    let selected = all
        .iter()
        .position(|r| r.issue_ref().is_some() && r.issue_ref() == list_state.selected);
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
        .map(|(i, row)| row_line(app, row, Some(i) == selected, list.width as usize, now))
        .collect();
    f.render_widget(Paragraph::new(lines), list);
    let mut layout = app.pr_layout.borrow_mut();
    layout.list = list;
    layout.first = first;
}

fn row_line(app: &App, row: &IssueRow, selected: bool, width: usize, now: i64) -> Line<'static> {
    let t = &app.theme;
    match row {
        IssueRow::Repo { repo, shown } => {
            let mut spans = vec![Span::styled(
                format!(" {}", repo.name),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            if let Some(why) = short_trouble(&repo.state) {
                spans.push(Span::styled(format!(" · {why}"), t.warn));
            } else if !repo.enabled {
                spans.push(Span::styled(" · issues off", t.dim));
            }
            let used: usize = spans.iter().map(Span::width).sum();
            let count = if repo.enabled {
                format!("{shown} ")
            } else {
                String::new()
            };
            spans.push(Span::raw(
                " ".repeat(width.saturating_sub(used + count.len())),
            ));
            spans.push(Span::styled(count, t.dim));
            Line::from(spans)
        }
        IssueRow::Nothing { repo } => {
            // Never read is not an answer yet.
            let what = match (&repo.fetched_at, &repo.state) {
                (None, GhState::Ok) => "   reading…",
                (None, _) => "   not read yet",
                _ if repo.issues.is_empty() => "   No open issues.",
                _ => "   nothing matches",
            };
            Line::from(Span::styled(what, t.dim))
        }
        IssueRow::More { more, .. } => {
            Line::from(Span::styled(format!("   +{more} more on GitHub"), t.dim))
        }
        IssueRow::Issue { issue, .. } => issue_line(t, issue, selected, width, now),
    }
}

/// `▌ #123  Login redirect loses the query   bug ui  @alice  3 comments  2d`.
fn issue_line(
    t: &Theme,
    issue: &IssueSummary,
    selected: bool,
    width: usize,
    now: i64,
) -> Line<'static> {
    let marker = if selected {
        Span::styled("▌", t.accent)
    } else {
        Span::raw(" ")
    };
    let number = Span::styled(format!(" #{:<4} ", issue.number), t.dim);
    let mut tail: Vec<Span> = vec![];
    if !issue.labels.is_empty() {
        tail.push(Span::styled(cut(&issue.labels.join(" "), 18), t.dim));
    }
    if let Some(first) = issue.assignees.first() {
        let more = match issue.assignees.len() {
            1 => String::new(),
            n => format!(" +{}", n - 1),
        };
        tail.push(Span::raw(format!("@{first}{more}")));
    }
    if issue.comments > 0 {
        let word = if issue.comments == 1 {
            "comment"
        } else {
            "comments"
        };
        tail.push(Span::styled(format!("{} {word}", issue.comments), t.dim));
    }
    let when = unix_secs(&issue.updated_at)
        .map(|u| age(now - u))
        .unwrap_or_default();
    let mut right: Vec<Span> = vec![];
    for s in tail {
        right.push(s);
        right.push(Span::raw("  "));
    }
    right.push(Span::styled(format!("{when:>3} "), t.dim));
    let right_width: usize = right.iter().map(Span::width).sum();
    let fixed = 1 + number.width() + 1 + right_width;
    let room = width.saturating_sub(fixed);
    let title = cut(&issue.title, room);
    let pad = room.saturating_sub(width_of(&title));
    let mut spans = vec![marker, number, Span::raw(title)];
    spans.push(Span::raw(" ".repeat(pad + 1)));
    spans.extend(right);
    if selected {
        for s in &mut spans {
            s.style = s.style.patch(t.selection);
        }
    }
    Line::from(spans)
}

/// The head of an issue: where it is, who opened it, its labels, who has it.
pub fn head_lines(
    t: &Theme,
    repo: &RepoIssues,
    issue: &IssueSummary,
    width: usize,
    now: i64,
) -> Vec<Line<'static>> {
    let when = unix_secs(&issue.created_at)
        .map(|u| format!(" {} ago", age(now - u)))
        .unwrap_or_default();
    let mut lines = vec![Line::from(Span::styled(
        cut(
            &format!("{} · opened by {}{when}", repo.slug, issue.author),
            width,
        ),
        t.dim,
    ))];
    let mut facts = vec![];
    if !issue.labels.is_empty() {
        facts.push(format!("labels {}", issue.labels.join(", ")));
    }
    if !issue.assignees.is_empty() {
        facts.push(format!("assigned {}", issue.assignees.join(", ")));
    }
    facts.push(match issue.comments {
        0 => "no comments".into(),
        1 => "1 comment".into(),
        n => format!("{n} comments"),
    });
    lines.push(Line::from(cut(&facts.join(" · "), width)));
    lines.push(Line::default());
    lines
}

/// The issue's description, drawn as markdown.
pub fn body_lines(t: &Theme, issue: &IssueSummary, width: u16) -> Vec<Line<'static>> {
    let body = strip_comments(&issue.body);
    if body.trim().is_empty() {
        return vec![Line::from(Span::styled("No description.", t.dim))];
    }
    render(&body, width, t)
}

/// `Space`: one issue on the whole view, scrolled.
fn draw_reading(
    f: &mut Frame,
    app: &App,
    view: &PrView,
    repo: &RepoIssues,
    issue: &IssueSummary,
    area: Rect,
) {
    let t = &app.theme;
    let w = area.width.saturating_sub(2) as usize;
    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} ",
        cut(
            &format!("#{} {}", issue.number, issue.title),
            w.saturating_sub(2)
        )
    ));
    let inner = block.inner(area);
    let mut lines = head_lines(t, repo, issue, w, app.now_secs());
    lines.extend(body_lines(t, issue, inner.width));
    let page = inner.height as usize;
    let end = lines.len().saturating_sub(page);
    let scroll = view.issues.reading.unwrap_or(0).min(end);
    let shown: Vec<Line> = lines.into_iter().skip(scroll).take(page).collect();
    f.render_widget(Paragraph::new(shown).block(block), area);
    let mut layout = app.pr_layout.borrow_mut();
    layout.body = inner;
    layout.end = end;
    layout.page = page;
    layout.scroll = scroll;
}

fn draw_preview(f: &mut Frame, app: &App, list: &IssueList, data: &ProjectIssues, area: Rect) {
    let t = &app.theme;
    let Some((repo, issue)) = list.selection(data) else {
        f.render_widget(Block::default().borders(Borders::ALL), area);
        return;
    };
    let w = area.width.saturating_sub(2) as usize;
    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} ",
        cut(
            &format!("#{} {}", issue.number, issue.title),
            w.saturating_sub(2)
        )
    ));
    let mut lines = head_lines(t, repo, issue, w, app.now_secs());
    lines.extend(body_lines(t, issue, w as u16));
    f.render_widget(Paragraph::new(lines).block(block), area);
}
