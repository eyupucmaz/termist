//! Mercek on screen: a head line, the file tree, and the diff of one file, unified or
//! split. A file's lines are worked out once and kept until something about them
//! changes (another file, width, layout, a thread folded); scrolling only moves them.
use super::render::{Cell, Row, rows};
use super::tree::{Node, TreeRow};
use super::{DiffArea, DiffView, Panel};
use crate::app::App;
use crate::prs::detail_view::viewed_mark;
use crate::prs::inbox_view::trouble;
use crate::prs::markdown::{cut, render, width_of};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use termist_core::AgentStatus;
use termist_core::config::DiffLayout;
use termist_core::diff::{LineKind, parse_patch};
use termist_core::github::{
    GhState, Patch, PrDetail, PrDiff, PrRef, Thread, Viewed, age, unix_secs,
};
use unicode_width::UnicodeWidthChar;

/// Below this many columns only the panel in use shows.
pub const BOTH_FROM: u16 = 100;
/// Below this many columns for the diff, split is drawn unified: each side would be
/// under 50.
pub const SPLIT_FROM: u16 = 100;
const TAB: &str = "    ";

/// A file's lines as drawn, with where its hunks and threads start.
#[derive(Clone, Default)]
struct Drawn {
    lines: Vec<Line<'static>>,
    hunks: Vec<usize>,
    threads: Vec<(usize, String)>,
}

thread_local! {
    static KEPT: RefCell<Option<(u64, Drawn)>> = const { RefCell::new(None) };
}

/// The layout drawn: split only where it fits.
pub fn layout(want: DiffLayout, width: u16) -> (DiffLayout, &'static str) {
    match want {
        DiffLayout::Split if width < SPLIT_FROM => (DiffLayout::Unified, "split · too narrow"),
        DiffLayout::Split => (DiffLayout::Split, "split"),
        DiffLayout::Unified => (DiffLayout::Unified, "unified"),
    }
}

pub fn draw(f: &mut Frame, app: &App, pr: PrRef, view: &DiffView, area: Rect) {
    let t = &app.theme;
    let detail = app.pr_details.get(&pr).and_then(|(_, d)| d.as_ref());
    let (state, diff) = match app.pr_diffs.get(&pr) {
        Some((state, diff)) => (state.clone(), diff.as_ref()),
        None => (GhState::Ok, None),
    };
    if area.height < 3 {
        return;
    }
    let both = area.width >= BOTH_FROM;
    let tree_w = if both {
        (area.width / 4).clamp(24, 40)
    } else if view.panel == Panel::Tree {
        area.width
    } else {
        0
    };
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    let tree_rect = Rect {
        width: tree_w,
        ..body
    };
    let diff_rect = Rect {
        x: body.x + tree_w,
        width: body.width - tree_w,
        ..body
    };
    let (drawn, label) = layout(app.config.diff.layout, diff_rect.width.saturating_sub(2));
    head(f, app, view, detail, diff, &state, label, area);
    let mut out = DiffArea::default();
    if tree_rect.width > 0 {
        draw_tree(f, t, view, detail, diff, tree_rect, &mut out);
    }
    if diff_rect.width > 0 {
        draw_diff(
            f, app, pr, view, detail, diff, &state, drawn, diff_rect, &mut out,
        );
    }
    app.pr_layout.borrow_mut().diff = out;
}

#[allow(clippy::too_many_arguments)]
fn head(
    f: &mut Frame,
    app: &App,
    view: &DiffView,
    detail: Option<&PrDetail>,
    diff: Option<&PrDiff>,
    state: &GhState,
    label: &str,
    area: Rect,
) {
    let t = &app.theme;
    let title = detail.map(|d| format!(" #{} {}", d.summary.number, d.summary.title));
    let mut right = vec![Span::styled(label.to_string(), t.dim)];
    if let Some(d) = diff {
        let seen = d
            .files
            .iter()
            .filter(|f| view.viewed(f) == Viewed::Viewed)
            .count();
        right.push(Span::styled(
            format!(" · {seen}/{} viewed", d.files.len()),
            t.dim,
        ));
    }
    if *state != GhState::Ok && diff.is_some() {
        right.push(Span::styled("  ⟳ failed", t.warn));
    }
    right.push(Span::raw(" "));
    let right_w: usize = right.iter().map(Span::width).sum();
    let room = (area.width as usize).saturating_sub(right_w + 1);
    let mut spans = vec![Span::styled(
        cut(&title.unwrap_or_default(), room),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::raw(
        " ".repeat((area.width as usize).saturating_sub(used + right_w)),
    ));
    spans.extend(right);
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
}

fn frame<'a>(t: &Theme, title: String, on: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(if on { t.focus } else { t.border })
        .title(title)
}

fn draw_tree(
    f: &mut Frame,
    t: &Theme,
    view: &DiffView,
    detail: Option<&PrDetail>,
    diff: Option<&PrDiff>,
    rect: Rect,
    out: &mut DiffArea,
) {
    let title = if view.typing || !view.query.is_empty() {
        format!(" /{} ", view.query)
    } else {
        " files ".to_string()
    };
    let block = frame(t, title, view.panel == Panel::Tree);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let Some(diff) = diff else {
        return;
    };
    let rows = view.rows(diff);
    let h = inner.height as usize;
    let first = view.cursor.saturating_sub(h.saturating_sub(1));
    let shown = view.index(diff);
    let w = inner.width as usize;
    let mut threads: HashMap<&str, usize> = HashMap::new();
    for th in detail.map_or(&[][..], |d| &d.threads) {
        *threads.entry(th.path.as_str()).or_default() += 1;
    }
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, r)| {
            let count = match r.node {
                Node::File { index, .. } => threads.get(diff.files[index].path.as_str()),
                Node::Dir { .. } => None,
            };
            let count = count.map(|n| n.to_string()).unwrap_or_default();
            tree_line(t, view, diff, r, shown, i == view.cursor, &count, w)
        })
        .collect();
    if diff.more > 0 && lines.len() < h {
        lines.push(Line::from(Span::styled(
            cut(&format!(" +{} more · b browser", diff.more), w),
            t.dim,
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
    out.tree = inner;
    out.tree_first = first;
}

#[allow(clippy::too_many_arguments)]
fn tree_line(
    t: &Theme,
    view: &DiffView,
    diff: &PrDiff,
    row: &TreeRow,
    shown: Option<usize>,
    cursor: bool,
    threads: &str,
    w: usize,
) -> Line<'static> {
    let indent = "  ".repeat(row.depth);
    let mut spans = match &row.node {
        Node::Dir { label, folded, .. } => vec![
            Span::raw(format!(" {indent}")),
            Span::styled(
                format!("{} {label}", if *folded { "▸" } else { "▾" }),
                t.dim,
            ),
        ],
        Node::File { index, label } => {
            let file = &diff.files[*index];
            let change = match file.change {
                'A' => Style::default().fg(t.status(AgentStatus::Finished)),
                'D' => t.error,
                _ => Style::default(),
            };
            let name = match (&file.previous, file.change) {
                (Some(old), 'R') => {
                    format!("{} → {label}", old.rsplit('/').next().unwrap_or(old))
                }
                _ => label.clone(),
            };
            let mut name_style = Style::default();
            if shown == Some(*index) {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let lead = format!(" {indent}");
            let room = w
                .saturating_sub(width_of(&lead) + 4)
                .saturating_sub(threads.len() + 1);
            let name = cut(&name, room);
            let pad = room.saturating_sub(width_of(&name));
            vec![
                Span::raw(lead),
                viewed_mark(t, view.viewed(file)),
                Span::styled(format!(" {} ", file.change), change),
                Span::styled(name, name_style),
                Span::raw(" ".repeat(pad + 1)),
                Span::styled(threads.to_string(), t.dim),
            ]
        }
    };
    if cursor && view.panel == Panel::Tree {
        let used: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
        for s in &mut spans {
            s.style = s.style.patch(t.selection);
        }
    }
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn draw_diff(
    f: &mut Frame,
    app: &App,
    pr: PrRef,
    view: &DiffView,
    detail: Option<&PrDetail>,
    diff: Option<&PrDiff>,
    state: &GhState,
    layout: DiffLayout,
    rect: Rect,
    out: &mut DiffArea,
) {
    let t = &app.theme;
    let file = diff.and_then(|d| Some(&d.files[view.index(d)?]));
    let title = match file {
        Some(file) => {
            let name = match (&file.previous, file.change) {
                (Some(old), 'R') => format!("{old} → {}", file.path),
                _ => file.path.clone(),
            };
            format!(
                " {}  {}  +{} −{} ",
                cut(&name, (rect.width as usize).saturating_sub(20)),
                file.change,
                file.additions,
                file.deletions
            )
        }
        None => " diff ".to_string(),
    };
    let block = frame(t, title, view.panel == Panel::Diff);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    out.body = inner;
    let say = |f: &mut Frame, text: String| {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {text}"), t.dim))),
            inner,
        );
    };
    let Some(file) = file else {
        let text = match (diff, trouble(state)) {
            (None, Some(why)) => why.join(" "),
            (None, None) => "Reading the diff…".to_string(),
            (Some(_), _) => "No file to show.".to_string(),
        };
        say(f, text);
        return;
    };
    let text = match &file.patch {
        Patch::Text(text) => text,
        Patch::Binary => return say(f, "binary file".into()),
        Patch::TooLarge => return say(f, "diff too large to show · b browser".into()),
        Patch::Renamed => return say(f, "renamed, no changes".into()),
    };
    let threads: Vec<&Thread> = detail
        .map(|d| d.threads.iter().filter(|th| th.path == file.path).collect())
        .unwrap_or_default();
    let width = inner.width as usize;
    let mut opened: Vec<&String> = view.opened.iter().collect();
    opened.sort();
    let mut hasher = DefaultHasher::new();
    (
        pr,
        diff.map(|d| d.head_oid.as_str()),
        &file.path,
        layout == DiffLayout::Split,
        width,
        view.hscroll,
        opened,
        threads
            .iter()
            .map(|th| (&th.id, th.comments.len()))
            .collect::<Vec<_>>(),
        t.diff_add,
        t.dim,
    )
        .hash(&mut hasher);
    let key = hasher.finish();
    let drawn = KEPT.with(|k| match &*k.borrow() {
        Some((kept, drawn)) if *kept == key => Some(drawn.clone()),
        _ => None,
    });
    let drawn = drawn.unwrap_or_else(|| {
        let drawn = lines(
            text,
            layout,
            &threads,
            &view.opened,
            width,
            view.hscroll,
            t,
            app.now_secs(),
        );
        KEPT.with(|k| *k.borrow_mut() = Some((key, drawn.clone())));
        drawn
    });
    let page = inner.height as usize;
    let end = drawn.lines.len().saturating_sub(page);
    let scroll = view.scroll.min(end);
    let shown: Vec<Line> = drawn.lines.into_iter().skip(scroll).take(page).collect();
    f.render_widget(Paragraph::new(shown), inner);
    out.end = end;
    out.page = page;
    out.hunks = drawn.hunks;
    out.threads = drawn.threads;
}

/// `text` with its `words` in `word` and the rest in `base`, tabs as spaces.
fn pieces(text: &str, words: &[Range<usize>], base: Style, word: Style) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let mut at = 0;
    for r in words {
        if r.start > at {
            out.push((text[at..r.start].replace('\t', TAB), base));
        }
        out.push((text[r.clone()].replace('\t', TAB), word));
        at = r.end;
    }
    if at < text.len() {
        out.push((text[at..].replace('\t', TAB), base));
    }
    out
}

/// `pieces` from column `skip`, `width` columns of them, filled with `fill` to the
/// width so a coloured line is coloured to its end; `…` last where the line goes on. A
/// wide character that does not fit is left out, and so is all that follows it.
fn window(
    pieces: &[(String, Style)],
    skip: usize,
    width: usize,
    fill: Style,
) -> Vec<Span<'static>> {
    let total: usize = pieces.iter().map(|(s, _)| width_of(s)).sum();
    let more = total > skip + width;
    let room = if more { width.saturating_sub(1) } else { width };
    let mut out = Vec::new();
    let (mut col, mut used, mut full) = (0usize, 0usize, false);
    for (text, style) in pieces {
        let mut kept = String::new();
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if col >= skip {
                if used + w > room {
                    full = true;
                    break;
                }
                kept.push(c);
                used += w;
            }
            col += w;
        }
        if !kept.is_empty() {
            out.push(Span::styled(kept, *style));
        }
        if full {
            break;
        }
    }
    if used < room {
        out.push(Span::styled(" ".repeat(room - used), fill));
    }
    if more {
        out.push(Span::styled("…", fill));
    }
    out
}

/// The styles of a line: its colour and its changed words'.
fn styles(t: &Theme, kind: LineKind) -> (Style, Style, &'static str) {
    match kind {
        LineKind::Add => (t.diff_add, t.diff_add_word, "+"),
        LineKind::Del => (t.diff_del, t.diff_del_word, "-"),
        LineKind::Context => (Style::default(), Style::default(), " "),
        LineKind::NoNewline => (t.dim, t.dim, "\\"),
    }
}

fn number(n: Option<u32>, w: usize) -> String {
    n.map_or(" ".repeat(w), |n| format!("{n:>w$}"))
}

/// One side's cell: `nw`-wide number (`old` or `new`), mark and text, `width` columns.
fn cell_spans(
    t: &Theme,
    c: &Cell,
    right: bool,
    nw: usize,
    width: usize,
    hscroll: usize,
) -> Vec<Span<'static>> {
    let n = if right { c.line.new } else { c.line.old };
    let (base, word, mark) = styles(t, c.line.kind);
    let text = match c.line.kind {
        LineKind::NoNewline => "No newline at end of file",
        _ => c.line.text.as_str(),
    };
    let mut spans = vec![
        Span::styled(format!(" {} ", number(n, nw)), t.dim),
        Span::styled(format!("{mark} "), base),
    ];
    let room = width.saturating_sub(nw + 4);
    spans.extend(window(
        &pieces(text, &c.words, base, word),
        hscroll,
        room,
        base,
    ));
    spans
}

/// Every line of a file's diff, `width` wide.
#[allow(clippy::too_many_arguments)]
fn lines(
    patch: &str,
    layout: DiffLayout,
    threads: &[&Thread],
    opened: &std::collections::HashSet<String>,
    width: usize,
    hscroll: usize,
    t: &Theme,
    now: i64,
) -> Drawn {
    let hunks = parse_patch(patch);
    let nw = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .flat_map(|l| [l.old, l.new])
        .flatten()
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let mut d = Drawn::default();
    for row in rows(&hunks, layout, threads) {
        match row {
            Row::Hunk(header) => {
                d.hunks.push(d.lines.len());
                d.lines.push(Line::from(Span::styled(
                    cut(&format!(" {header}"), width),
                    t.dim,
                )));
            }
            Row::Unified(c) => {
                let (base, word, mark) = styles(t, c.line.kind);
                let text = match c.line.kind {
                    LineKind::NoNewline => "No newline at end of file",
                    _ => c.line.text.as_str(),
                };
                let mut spans = vec![Span::styled(
                    format!(" {} {} │", number(c.line.old, nw), number(c.line.new, nw)),
                    t.dim,
                )];
                spans.push(Span::styled(format!(" {mark} "), base));
                let room = width.saturating_sub(2 * nw + 6);
                spans.extend(window(
                    &pieces(text, &c.words, base, word),
                    hscroll,
                    room,
                    base,
                ));
                d.lines.push(Line::from(spans));
            }
            Row::Split { left, right } => {
                let lw = width.saturating_sub(1) / 2;
                let rw = width.saturating_sub(1 + lw);
                let side = |c: &Option<Cell>, right: bool, w: usize| match c {
                    Some(c) => cell_spans(t, c, right, nw, w, hscroll),
                    None => vec![Span::raw(" ".repeat(w))],
                };
                let mut spans = side(&left, false, lw);
                spans.push(Span::styled("│", t.border));
                spans.extend(side(&right, true, rw));
                d.lines.push(Line::from(spans));
            }
            Row::Thread(i) => {
                let th = threads[i];
                d.threads.push((d.lines.len(), th.id.clone()));
                thread_lines(&mut d.lines, th, opened.contains(&th.id), width, t, now);
            }
            Row::Outdated => {
                d.lines.push(Line::default());
                d.lines.push(Line::from(Span::styled(
                    " outdated",
                    t.dim.add_modifier(Modifier::BOLD),
                )));
            }
        }
    }
    d
}

fn thread_lines(
    out: &mut Vec<Line<'static>>,
    th: &Thread,
    open: bool,
    width: usize,
    t: &Theme,
    now: i64,
) {
    let bar = || Span::styled(" ┃ ", t.accent);
    let status = if th.resolved {
        "resolved"
    } else if th.outdated {
        "outdated"
    } else {
        "open"
    };
    let first = th.comments.first();
    let author = first.map_or("ghost", |c| c.author.as_str());
    let others = th.comments.len().saturating_sub(1) as u32 + th.more;
    let others = if others > 0 {
        format!(" +{others}")
    } else {
        String::new()
    };
    let mark = if open { "▾" } else { "▸" };
    let gist = first
        .map(|c| {
            c.body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
        })
        .unwrap_or("");
    let head = format!("{mark} {author}{others} · {status}");
    let room = width.saturating_sub(3 + width_of(&head) + 3);
    let mut spans = vec![bar(), Span::styled(head, t.accent)];
    if !open && !gist.is_empty() {
        spans.push(Span::styled(format!(" · {}", cut(gist, room)), t.dim));
    }
    out.push(Line::from(spans));
    if !open {
        return;
    }
    let room = width.saturating_sub(5).max(8) as u16;
    for c in &th.comments {
        let ago = unix_secs(&c.created_at)
            .map(|u| format!(" · {} ago", age(now - u)))
            .unwrap_or_default();
        out.push(Line::from(vec![
            bar(),
            Span::styled(
                c.author.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(ago, t.dim),
        ]));
        for line in render(&c.body, room, t) {
            let mut spans = vec![bar(), Span::raw("  ")];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
    }
    if th.more > 0 {
        out.push(Line::from(vec![
            bar(),
            Span::styled(format!("+{} more · b browser", th.more), t.dim),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_long_line_is_cut_and_moved_by_columns_wide_characters_too() {
        let base = Style::default();
        let p = pieces("ab\tçiçek 漢字漢字 end", &[], base, base);
        assert_eq!(text(&window(&p, 0, 10, base)), "ab    çiç…");
        assert_eq!(
            text(&window(&p, 6, 8, base)),
            "çiçek  …",
            "a wide one that does not fit is left out, with all after it"
        );
        assert_eq!(
            text(&window(&p, 0, 40, base)).trim_end(),
            "ab    çiçek 漢字漢字 end"
        );
        assert_eq!(
            width_of(&text(&window(&p, 3, 13, base))),
            13,
            "always as wide as asked"
        );
    }

    #[test]
    fn changed_words_keep_their_place_after_tabs() {
        let (base, word) = (
            Style::default(),
            Style::default().add_modifier(Modifier::BOLD),
        );
        let p = pieces("\tx = 1", &[Range { start: 5, end: 6 }], base, word);
        assert_eq!(p, [("    x = ".to_string(), base), ("1".to_string(), word)]);
    }

    #[test]
    fn split_falls_back_where_it_does_not_fit() {
        assert_eq!(
            layout(DiffLayout::Split, 99),
            (DiffLayout::Unified, "split · too narrow")
        );
        assert_eq!(layout(DiffLayout::Split, 100), (DiffLayout::Split, "split"));
        assert_eq!(
            layout(DiffLayout::Unified, 300),
            (DiffLayout::Unified, "unified")
        );
    }
}
