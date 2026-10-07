//! One pull request whole: its head, then Overview, Conversation, Checks or Files.
use super::inbox_view::{status_line, trouble};
use super::markdown::{cut, render, width_of};
use super::{Detail, Tab, timeline};
use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use termist_core::AgentStatus;
use termist_core::github::{Check, CheckState, GhState, PrDetail, Viewed, age, unix_secs};

pub fn draw(f: &mut Frame, app: &App, d: &Detail, area: Rect) {
    let t = &app.theme;
    let now = app.now_secs();
    let w = area.width as usize;
    let (state, detail) = match app.pr_details.get(&d.pr) {
        Some((state, detail)) => (state.clone(), detail.as_ref()),
        None => (GhState::Ok, None),
    };
    let s = detail.map_or(&d.summary, |x| &x.summary);
    let opened = unix_secs(&s.created_at)
        .map(|u| format!(" · opened {} ago", age(now - u)))
        .unwrap_or_default();
    let mut head = vec![
        Line::from(Span::styled(
            cut(&format!(" {}", s.title), w),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            cut(
                &format!(
                    " {} wants to merge {} into {} · +{} −{} · {} files{opened}",
                    s.author, s.head, s.base, s.additions, s.deletions, s.changed_files
                ),
                w,
            ),
            t.dim,
        )),
    ];
    let mut status = status_line(t, s);
    status.spans.insert(0, Span::raw(" "));
    if let Some(mark) = detail.and_then(|x| super::pending_mark(t, x)) {
        status.spans.push(Span::raw("    "));
        status.spans.push(mark);
    }
    if let Some(why) = trouble(&state) {
        status
            .spans
            .push(Span::styled(format!("  ⟳ {}", why.join(" ")), t.warn));
    }
    head.push(status);
    let tab_row = area.y + head.len() as u16;
    let mut places = vec![];
    let mut tabs = vec![Span::raw(" ")];
    for tab in Tab::ALL {
        let count = detail.and_then(|x| match tab {
            Tab::Overview => None,
            Tab::Conversation => Some(timeline::entries(x).len()),
            Tab::Checks => Some(x.checks.len()),
            Tab::Files => Some(x.files.len()),
        });
        let label = match count {
            Some(n) => format!(" {} {n} ", tab.label()),
            None => format!(" {} ", tab.label()),
        };
        let style = if tab == d.tab { t.tab_active } else { t.dim };
        let from = area.x + tabs.iter().map(Span::width).sum::<usize>() as u16;
        places.push((tab, from, from + width_of(&label) as u16));
        tabs.push(Span::styled(label, style));
        tabs.push(Span::raw(" "));
    }
    head.push(Line::from(tabs));
    head.push(Line::from(Span::styled("─".repeat(w), t.border)));
    let head_h = (head.len() as u16).min(area.height);
    f.render_widget(
        Paragraph::new(head),
        Rect {
            height: head_h,
            ..area
        },
    );
    let body = Rect {
        y: area.y + head_h,
        height: area.height - head_h,
        ..area
    };
    let mut files = vec![];
    let (lines, threads, checks) = match detail {
        None => {
            let text = trouble(&state)
                .map(|w| w.join(" "))
                .unwrap_or_else(|| "Reading the pull request…".into());
            (
                vec![Line::from(Span::styled(format!(" {text}"), t.dim))],
                vec![],
                vec![],
            )
        }
        Some(x) => match d.tab {
            Tab::Overview => (overview(x, w, t), vec![], vec![]),
            Tab::Conversation => {
                let none = std::collections::BTreeSet::new();
                let marked = app.marks.get(&d.pr).unwrap_or(&none);
                let (mut lines, anchors) = timeline::lines(x, w, &d.toggled, marked, t, now);
                // The highlighted item: its bar in the focus colour.
                if let Some(a) = anchors.get(d.item.min(anchors.len().saturating_sub(1))) {
                    let end = anchors
                        .iter()
                        .map(|x| x.line)
                        .find(|l| *l > a.line)
                        .unwrap_or(lines.len());
                    for l in &mut lines[a.line..end] {
                        if let Some(bar) =
                            l.spans.first_mut().filter(|s| s.content.starts_with('┃'))
                        {
                            bar.style = t.focus;
                        }
                    }
                }
                (lines, anchors, vec![])
            }
            Tab::Checks => {
                let (lines, urls) = check_lines(x, d.check, t);
                (lines, vec![], urls)
            }
            Tab::Files => {
                files = x.files.iter().map(|f| f.path.clone()).collect();
                let at = d
                    .file
                    .as_ref()
                    .and_then(|p| files.iter().position(|f| f == p))
                    .unwrap_or(0);
                (file_lines(x, at, w, t), vec![], vec![])
            }
        },
    };
    let page = body.height as usize;
    let end = lines.len().saturating_sub(page);
    let scroll = match d.tab {
        Tab::Checks => d.check.saturating_sub(page.saturating_sub(1)).min(end),
        Tab::Files => {
            let at = d
                .file
                .as_ref()
                .and_then(|p| files.iter().position(|f| f == p))
                .unwrap_or(0);
            at.saturating_sub(page.saturating_sub(1)).min(end)
        }
        _ => d.scroll.min(end),
    };
    let shown: Vec<Line> = lines.into_iter().skip(scroll).take(page).collect();
    f.render_widget(Paragraph::new(shown), body);
    let mut layout = app.pr_layout.borrow_mut();
    layout.end = end;
    layout.page = page;
    layout.items = threads;
    layout.checks = checks;
    layout.files = files;
    layout.tabs = places;
    layout.tab_row = tab_row;
    layout.body = body;
    layout.scroll = scroll;
}

fn overview(d: &PrDetail, w: usize, t: &Theme) -> Vec<Line<'static>> {
    if d.body.trim().is_empty() {
        return vec![Line::from(Span::styled(" no description", t.dim))];
    }
    render(&d.body, w.saturating_sub(1) as u16, t)
        .into_iter()
        .map(|l| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect()
}

fn check_rank(state: CheckState) -> u8 {
    match state {
        CheckState::Failed => 0,
        CheckState::Running => 1,
        CheckState::Queued => 2,
        CheckState::Passed => 3,
        CheckState::Neutral => 4,
        CheckState::Skipped => 5,
        CheckState::Cancelled => 6,
    }
}

fn duration(c: &Check) -> Option<String> {
    let secs = unix_secs(c.completed_at.as_deref()?)? - unix_secs(c.started_at.as_deref()?)?;
    Some(if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    })
}

fn check_lines(
    d: &PrDetail,
    highlight: usize,
    t: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<String>>) {
    let green = Style::default().fg(t.status(AgentStatus::Unseen));
    let mut checks: Vec<&Check> = d.checks.iter().collect();
    checks.sort_by_key(|c| check_rank(c.state));
    let lines = checks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (mark, style) = match c.state {
                CheckState::Passed => ("✓", green),
                CheckState::Failed => ("✗", t.error),
                CheckState::Running => ("◐", Style::default().fg(t.status(AgentStatus::Running))),
                CheckState::Queued => ("○", t.dim),
                _ => ("·", t.dim),
            };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(format!("{mark} "), style),
                Span::raw(c.name.clone()),
            ];
            if let Some(w) = &c.workflow {
                spans.push(Span::styled(format!(" · {w}"), t.dim));
            }
            if let Some(took) = duration(c) {
                spans.push(Span::styled(format!(" · {took}"), t.dim));
            }
            if i == highlight {
                for s in &mut spans {
                    s.style = s.style.patch(t.selection);
                }
            }
            Line::from(spans)
        })
        .collect();
    (lines, checks.iter().map(|c| c.url.clone()).collect())
}

/// `✓` viewed, `◦` changed since it was viewed, nothing otherwise.
pub fn viewed_mark(t: &Theme, viewed: Viewed) -> Span<'static> {
    match viewed {
        Viewed::Viewed => Span::styled("✓", Style::default().fg(t.status(AgentStatus::Finished))),
        Viewed::Dismissed => Span::styled("◦", t.warn),
        Viewed::Unviewed => Span::raw(" "),
    }
}

fn file_lines(d: &PrDetail, highlight: usize, w: usize, t: &Theme) -> Vec<Line<'static>> {
    let green = Style::default().fg(t.status(AgentStatus::Unseen));
    let mut lines: Vec<Line> = d
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let counts = format!("+{} −{}", f.additions, f.deletions);
            let room = w.saturating_sub(6 + counts.len() + 2);
            let path = cut(&f.path, room);
            let pad = room.saturating_sub(super::markdown::width_of(&path));
            let mut spans = vec![
                Span::raw(" "),
                viewed_mark(t, f.viewed),
                Span::styled(format!(" {} ", f.change), t.accent),
                Span::raw(path),
                Span::raw(" ".repeat(pad + 1)),
                Span::styled(format!("+{}", f.additions), green),
                Span::raw(" "),
                Span::styled(format!("−{}", f.deletions), t.error),
            ];
            if i == highlight {
                for s in &mut spans {
                    s.style = s.style.patch(t.selection);
                }
            }
            Line::from(spans)
        })
        .collect();
    if d.more.files > 0 {
        lines.push(Line::from(Span::styled(
            format!(" +{} more files · b browser", d.more.files),
            t.dim,
        )));
    }
    lines
}
