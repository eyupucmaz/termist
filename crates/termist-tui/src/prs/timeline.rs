//! A pull request's conversation: comments, reviews and line threads, in time order.
use super::markdown::{cut, render};
use super::{Item, Mine};
use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use termist_core::AgentStatus;
use termist_core::github::{
    Comment, CommentKind, PrDetail, Review, ReviewState, Thread, age, unix_secs,
};

#[derive(Clone, Copy, Debug)]
pub enum Entry<'a> {
    Comment(&'a Comment),
    Review(&'a Review),
    Thread(&'a Thread),
}

impl Entry<'_> {
    pub fn at(&self) -> &str {
        match self {
            Entry::Comment(c) => &c.created_at,
            Entry::Review(r) => &r.submitted_at,
            Entry::Thread(t) => t.comments.first().map_or("", |c| c.created_at.as_str()),
        }
    }
}

pub fn entries(d: &PrDetail) -> Vec<Entry<'_>> {
    let mut all: Vec<Entry> = d.comments.iter().map(Entry::Comment).collect();
    all.extend(
        d.reviews
            .iter()
            .filter(|r| {
                r.state != ReviewState::Pending
                    && !(r.state == ReviewState::Commented && r.body.trim().is_empty())
            })
            .map(Entry::Review),
    );
    all.extend(d.threads.iter().map(Entry::Thread));
    all.sort_by(|a, b| a.at().cmp(b.at()));
    all
}

pub fn hunk_tail(hunk: &str, n: usize) -> Vec<(Option<u32>, char, String)> {
    let mut next: Option<u32> = None;
    let mut out = Vec::new();
    for line in hunk.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            next = rest
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|part| part.split(',').next())
                .and_then(|start| start.parse().ok());
            continue;
        }
        let mark = line.chars().next().unwrap_or(' ');
        let text = line.get(mark.len_utf8()..).unwrap_or("").to_string();
        let number = match mark {
            '-' | '\\' => None,
            _ => {
                let here = next;
                next = next.map(|n| n + 1);
                here
            }
        };
        out.push((number, mark, text));
    }
    let keep = out.len().saturating_sub(n);
    out.split_off(keep)
}

/// `c` as yours to edit or delete, if it is.
fn mine(c: &Comment, kind: CommentKind) -> Option<Mine> {
    c.mine.then(|| Mine {
        id: c.id.clone(),
        kind,
        body: c.body.clone(),
        can_edit: c.can_edit,
        can_delete: c.can_delete,
    })
}

pub fn lines(
    d: &PrDetail,
    width: usize,
    toggled: &HashSet<String>,
    t: &Theme,
    now: i64,
) -> (Vec<Line<'static>>, Vec<Item>) {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let green = Style::default().fg(t.status(AgentStatus::Unseen));
    let ago = |at: &str| {
        unix_secs(at)
            .map(|u| format!(" · {} ago", age(now - u)))
            .unwrap_or_default()
    };
    let bar = || Span::styled("┃ ", t.accent);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut anchors = Vec::new();
    let body = |out: &mut Vec<Line<'static>>, text: &str, indent: &str| {
        let room = width.saturating_sub(2 + indent.len()).max(8) as u16;
        for line in render(text, room, t) {
            let mut spans = vec![bar(), Span::raw(indent.to_string())];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
    };
    for entry in entries(d) {
        match entry {
            Entry::Comment(c) => {
                anchors.push(Item {
                    line: out.len(),
                    mine: mine(c, CommentKind::Issue),
                    ..Item::default()
                });
                out.push(Line::from(vec![
                    bar(),
                    Span::styled(c.author.clone(), bold),
                    Span::styled(ago(&c.created_at), t.dim),
                ]));
                body(&mut out, &c.body, "  ");
            }
            Entry::Review(r) => {
                let (mark, words, style) = match r.state {
                    ReviewState::Approved => ("✓", "approved", green),
                    ReviewState::ChangesRequested => ("✗", "requested changes", t.error),
                    ReviewState::Dismissed => ("·", "review dismissed", t.dim),
                    _ => ("·", "reviewed", t.dim),
                };
                anchors.push(Item {
                    line: out.len(),
                    ..Item::default()
                });
                out.push(Line::from(vec![
                    bar(),
                    Span::styled(format!("{mark} "), style),
                    Span::styled(r.author.clone(), bold),
                    Span::raw(format!(" {words}")),
                    Span::styled(ago(&r.submitted_at), t.dim),
                ]));
                if !r.body.trim().is_empty() {
                    body(&mut out, &r.body, "  ");
                }
            }
            Entry::Thread(th) => {
                let folded = (th.resolved || th.outdated) != toggled.contains(&th.id);
                let place = match th.line {
                    Some(n) => format!("{}:{n}", th.path),
                    None => th.path.clone(),
                };
                let status = if th.resolved {
                    "resolved"
                } else if th.outdated {
                    "outdated"
                } else {
                    "open"
                };
                let count = th.comments.len() as u32 + th.more;
                let plural = if count == 1 { "" } else { "s" };
                anchors.push(Item {
                    line: out.len(),
                    thread: Some(th.id.clone()),
                    open: !th.resolved,
                    can_reply: th.can_reply,
                    can_resolve: th.can_resolve,
                    resolved: th.resolved,
                    mine: th
                        .comments
                        .iter()
                        .rev()
                        .find(|c| c.mine)
                        .and_then(|c| mine(c, CommentKind::Review)),
                });
                let mut head = vec![
                    bar(),
                    Span::styled(place, t.accent),
                    Span::styled(format!(" · {status} · {count} comment{plural}"), t.dim),
                ];
                if folded {
                    head.push(Span::styled("  ▸ Enter", t.dim));
                }
                out.push(Line::from(head));
                if folded {
                    out.push(Line::default());
                    continue;
                }
                for (number, mark, text) in hunk_tail(&th.hunk, 3) {
                    let style = match mark {
                        '+' => green,
                        '-' => t.error,
                        _ => t.dim,
                    };
                    let number = number.map(|n| n.to_string()).unwrap_or_default();
                    out.push(Line::from(vec![
                        bar(),
                        Span::styled(format!("  {number:>4} │ "), t.dim),
                        Span::styled(
                            cut(&format!("{mark}{text}"), width.saturating_sub(12)),
                            style,
                        ),
                    ]));
                }
                for c in &th.comments {
                    let mut head = vec![
                        bar(),
                        Span::raw("  "),
                        Span::styled(c.author.clone(), bold),
                        Span::styled(ago(&c.created_at), t.dim),
                    ];
                    if c.pending {
                        head.push(Span::styled(" · pending", t.warn));
                    }
                    out.push(Line::from(head));
                    body(&mut out, &c.body, "    ");
                }
                if th.more > 0 {
                    out.push(Line::from(vec![
                        bar(),
                        Span::styled(format!("    +{} more · b browser", th.more), t.dim),
                    ]));
                }
            }
        }
        out.push(Line::default());
    }
    let more = d.more.comments + d.more.reviews + d.more.threads;
    if more > 0 {
        out.push(Line::from(Span::styled(
            format!("+{more} more on GitHub · b browser"),
            t.dim,
        )));
    }
    (out, anchors)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{detail, summary};
    use super::*;

    #[test]
    fn entries_go_by_time_and_empty_comment_reviews_are_left_out() {
        let d = detail(summary(212, "x", "bob"));
        let order: Vec<String> = entries(&d)
            .iter()
            .map(|e| match e {
                Entry::Comment(c) => format!("comment {}", c.author),
                Entry::Review(r) => format!("review {}", r.author),
                Entry::Thread(t) => format!("thread {}", t.id),
            })
            .collect();
        assert_eq!(
            order,
            ["thread T2", "comment bob", "thread T1", "review carol"]
        );
    }

    #[test]
    fn a_hunk_tail_carries_new_line_numbers() {
        assert_eq!(
            hunk_tail("@@ -10,4 +20,4 @@\n a\n-b\n+c\n d", 3),
            [
                (None, '-', "b".to_string()),
                (Some(21), '+', "c".to_string()),
                (Some(22), ' ', "d".to_string()),
            ]
        );
        assert!(hunk_tail("", 3).is_empty());
    }

    #[test]
    fn resolved_threads_start_folded_and_anchors_point_at_threads() {
        let d = detail(summary(212, "x", "bob"));
        let t = Theme::terminal();
        let now = unix_secs("2026-10-02T12:00:00Z").unwrap();
        let (shown, anchors) = lines(&d, 80, &HashSet::new(), &t, now);
        let text: Vec<String> = shown
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        // A comment, two threads, an approval; the empty "commented" review is not one.
        let threads: Vec<&super::Item> = anchors.iter().filter(|a| a.thread.is_some()).collect();
        assert_eq!((anchors.len(), threads.len()), (4, 2));
        assert_eq!(
            (threads[0].thread.as_deref(), threads[0].open),
            (Some("T2"), false)
        );
        assert!(text[threads[0].line].contains("src/api/client.ts:10 · resolved · 1 comment"));
        assert!(text[threads[0].line].contains("▸ Enter"), "folded");
        let t1 = &text[threads[1].line];
        assert!(
            t1.contains("src/search/DealerFilter.tsx:42 · open · 2 comments"),
            "{t1}"
        );
        assert!(
            text.iter().any(|l| l.contains("42 │ + useEffect")),
            "{text:#?}"
        );
        assert!(
            !text.iter().any(|l| l.trim_end().ends_with(" nit")),
            "the folded thread hides its comments"
        );
        let (lines, _) = lines(&d, 80, &HashSet::from(["T2".to_string()]), &t, now);
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.trim() == "nit"))
        );
    }
}
