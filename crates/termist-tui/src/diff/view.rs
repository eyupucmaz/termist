//! Mercek on screen: a head line, the file tree, and the diff of one file, unified or
//! split. A file's lines are worked out once and kept until something about them
//! changes (another file, width, layout, a thread folded); scrolling only moves them.
use super::render::{Cell, Row, rows};
use super::tree::{Node, TreeRow};
use super::{DiffArea, DiffView, Panel, Spot, follow};
use crate::app::App;
use crate::prs::detail_view::viewed_mark;
use crate::prs::markdown::{cut, render, width_of};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use termist_core::AgentStatus;
use termist_core::config::DiffLayout;
use termist_core::diff::{LineKind, parse_patch};
use termist_core::github::{DiffFile, Patch, Side, Thread, Viewed, age, unix_secs};
use unicode_width::UnicodeWidthChar;

/// Below this many columns only the panel in use shows.
pub const BOTH_FROM: u16 = 100;
/// Below this many columns for the diff, split is drawn unified: each side would be
/// under 50.
pub const SPLIT_FROM: u16 = 100;
const TAB: &str = "    ";

/// A file's lines as drawn, with where its hunks and threads start.
#[derive(Default)]
struct Drawn {
    lines: Vec<Line<'static>>,
    hunks: Vec<usize>,
    threads: Vec<(usize, String)>,
    /// What each line is.
    spots: Rc<Vec<Spot>>,
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

/// What the diff view draws, from a pull request or a folder.
pub struct Shown<'a> {
    /// Tells this diff's files apart from another's (or an older read's) in the cache
    /// of drawn lines.
    pub id: u64,
    /// `None` until read.
    pub files: Option<&'a [DiffFile]>,
    /// Changed files past those read.
    pub more: u32,
    pub threads: &'a [Thread],
    /// Threads marked for an agent (`Space`).
    pub marked: &'a BTreeSet<String>,
    /// The head line's left side, and what goes on its right before the layout.
    pub title: String,
    pub badges: Vec<Span<'static>>,
    /// What a file `Ctrl+r` marked is: `viewed`, or `reviewed`.
    pub seen: &'static str,
    /// The newest read failed while an older one is shown.
    pub failed: bool,
    /// Said where the file would be before anything was read: reading, or why not.
    pub waiting: String,
    /// Said after `+N more` and a file too large: where the rest can be seen.
    pub elsewhere: &'static str,
}

pub fn draw(f: &mut Frame, app: &App, shown: &Shown, view: &DiffView, area: Rect) {
    let t = &app.theme;
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
    head(f, app, view, shown, label, area);
    let mut out = DiffArea::default();
    if tree_rect.width > 0 {
        draw_tree(f, t, view, shown, tree_rect, &mut out);
    }
    if diff_rect.width > 0 {
        draw_diff(f, app, view, shown, drawn, diff_rect, &mut out);
    }
    app.pr_layout.borrow_mut().diff = out;
}

fn head(f: &mut Frame, app: &App, view: &DiffView, shown: &Shown, label: &str, area: Rect) {
    let t = &app.theme;
    let mut right = shown.badges.clone();
    right.push(Span::styled(label.to_string(), t.dim));
    if let Some(files) = shown.files {
        let seen = files
            .iter()
            .filter(|f| view.viewed(f) == Viewed::Viewed)
            .count();
        right.push(Span::styled(
            format!(" · {seen}/{} {}", files.len(), shown.seen),
            t.dim,
        ));
    }
    if shown.failed && shown.files.is_some() {
        right.push(Span::styled("  ⟳ failed", t.warn));
    }
    right.push(Span::raw(" "));
    let right_w: usize = right.iter().map(Span::width).sum();
    let room = (area.width as usize).saturating_sub(right_w + 1);
    let mut spans = vec![Span::styled(
        cut(&shown.title, room),
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
    shown: &Shown,
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
    let Some(diff) = shown.files else {
        return;
    };
    let rows = view.rows(diff);
    let h = inner.height as usize;
    let first = view.cursor.saturating_sub(h.saturating_sub(1));
    let open = view.index(diff);
    let w = inner.width as usize;
    let mut threads: HashMap<&str, usize> = HashMap::new();
    for th in shown.threads {
        *threads.entry(th.path.as_str()).or_default() += 1;
    }
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(h)
        .map(|(i, r)| {
            let count = match r.node {
                Node::File { index, .. } => threads.get(diff[index].path.as_str()),
                Node::Dir { .. } => None,
            };
            let count = count.map(|n| n.to_string()).unwrap_or_default();
            tree_line(t, view, diff, r, open, i == view.cursor, &count, w)
        })
        .collect();
    if shown.more > 0 && lines.len() < h {
        lines.push(Line::from(Span::styled(
            cut(&format!(" +{} more{}", shown.more, shown.elsewhere), w),
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
    diff: &[DiffFile],
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
            let file = &diff[*index];
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

fn draw_diff(
    f: &mut Frame,
    app: &App,
    view: &DiffView,
    shown: &Shown,
    layout: DiffLayout,
    rect: Rect,
    out: &mut DiffArea,
) {
    let t = &app.theme;
    let diff = shown.files;
    let file = diff.and_then(|d| Some(&d[view.index(d)?]));
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
        let text = match diff {
            None => shown.waiting.clone(),
            Some(_) => "No file to show.".to_string(),
        };
        say(f, text);
        return;
    };
    let text = match &file.patch {
        Patch::Text(text) => text,
        Patch::Binary => return say(f, "binary file".into()),
        Patch::TooLarge => return say(f, format!("diff too large to show{}", shown.elsewhere)),
        Patch::Renamed => return say(f, "renamed, no changes".into()),
    };
    let threads: Vec<&Thread> = shown
        .threads
        .iter()
        .filter(|th| th.path == file.path)
        .collect();
    let width = inner.width as usize;
    let mut opened: Vec<&String> = view.opened.iter().collect();
    opened.sort();
    let marked = shown.marked;
    let mut hasher = DefaultHasher::new();
    (
        shown.id,
        &file.path,
        layout == DiffLayout::Split,
        width,
        view.hscroll,
        opened,
        marked,
        threads
            .iter()
            // A comment sent or written turns its thread over.
            .map(|th| {
                (
                    &th.id,
                    th.comments.len(),
                    th.comments.iter().filter(|c| c.pending).count(),
                )
            })
            .collect::<Vec<_>>(),
        t.diff_add,
        t.dim,
    )
        .hash(&mut hasher);
    let key = hasher.finish();
    let page = inner.height as usize;
    // The file's lines stay in the cache; a frame copies only the page it shows.
    let (mut visible, top, end, hunks, threads, spots) = KEPT.with(|k| {
        let mut kept = k.borrow_mut();
        let stale = !matches!(&*kept, Some((at, _)) if *at == key);
        if stale {
            let drawn = lines(
                text,
                layout,
                &threads,
                &view.opened,
                marked,
                width,
                view.hscroll,
                t,
                app.now_secs(),
            );
            *kept = Some((key, drawn));
        }
        let drawn = &kept.as_ref().expect("filled above").1;
        let end = drawn.lines.len().saturating_sub(page);
        // The screen follows the cursor.
        let top = follow(view.line, view.scroll, page).min(end);
        let visible: Vec<Line> = drawn.lines[top..].iter().take(page).cloned().collect();
        (
            visible,
            top,
            end,
            drawn.hunks.clone(),
            drawn.threads.clone(),
            drawn.spots.clone(),
        )
    });
    // The cursor, and the range from `v`.
    let (lo, hi) = match view.anchor {
        Some(a) => (a.min(view.line), a.max(view.line)),
        None => (view.line, view.line),
    };
    let mark = if view.panel == Panel::Diff {
        t.selection
    } else {
        t.dim.add_modifier(Modifier::REVERSED)
    };
    for (i, l) in visible.iter_mut().enumerate() {
        if (lo..=hi).contains(&(top + i)) {
            for s in &mut l.spans {
                s.style = s.style.patch(mark);
            }
        }
    }
    f.render_widget(Paragraph::new(visible), inner);
    out.end = end;
    out.page = page;
    out.hunks = hunks;
    out.threads = threads;
    out.spots = spots;
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
    marked: &std::collections::BTreeSet<String>,
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
    let mut spots: Vec<Spot> = Vec::new();
    // The hunk the rows are in; none once in the outdated threads.
    let mut hunk: Option<usize> = None;
    for row in rows(&hunks, layout, threads) {
        match row {
            Row::Hunk(header) => {
                let n = hunk.map_or(0, |h| h + 1);
                hunk = Some(n);
                spots.push(Spot::Hunk(n));
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
                spots.push(line_spot(hunk, Some(&c), None));
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
                spots.push(line_spot(hunk, left.as_ref(), right.as_ref()));
            }
            Row::Thread(i) => {
                let th = threads[i];
                d.threads.push((d.lines.len(), th.id.clone()));
                let before = d.lines.len();
                let (open, mark) = (opened.contains(&th.id), marked.contains(&th.id));
                thread_lines(&mut d.lines, th, open, mark, width, t, now);
                spots.extend((before..d.lines.len()).map(|_| Spot::Thread {
                    hunk,
                    id: th.id.clone(),
                }));
            }
            Row::Outdated => {
                hunk = None;
                spots.extend([Spot::Other { hunk: None }, Spot::Other { hunk: None }]);
                d.lines.push(Line::default());
                d.lines.push(Line::from(Span::styled(
                    " outdated",
                    t.dim.add_modifier(Modifier::BOLD),
                )));
            }
        }
    }
    d.spots = Rc::new(spots);
    d
}

/// What a comment on a drawn line takes. Unified: the line itself (`left` alone).
/// Split: the new side when the row has one, else the deleted line on the old side.
fn line_spot(hunk: Option<usize>, left: Option<&Cell>, right: Option<&Cell>) -> Spot {
    let hunk_or_other = |s: Spot| match hunk {
        Some(_) => s,
        None => Spot::Other { hunk: None },
    };
    let pick = match (left, right) {
        (_, Some(r)) if r.line.kind != LineKind::NoNewline => Some(r),
        (Some(l), _) => Some(l),
        _ => None,
    };
    let Some(c) = pick.filter(|c| c.line.kind != LineKind::NoNewline) else {
        return Spot::Other { hunk };
    };
    let (side, number) = match (c.line.kind, c.line.new, c.line.old) {
        (LineKind::Del, _, Some(old)) => (Side::Left, old),
        (_, Some(new), _) => (Side::Right, new),
        _ => return Spot::Other { hunk },
    };
    let mark = match c.line.kind {
        LineKind::Add => '+',
        LineKind::Del => '-',
        _ => ' ',
    };
    hunk_or_other(Spot::Line {
        hunk: hunk.unwrap_or(0),
        side,
        number,
        mark,
        text: c.line.text.clone(),
        new: (side == Side::Right).then(|| c.line.text.clone()),
    })
}

fn thread_lines(
    out: &mut Vec<Line<'static>>,
    th: &Thread,
    open: bool,
    marked: bool,
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
    let pending = if first.is_some_and(|c| c.pending) {
        " · pending"
    } else {
        ""
    };
    let head = format!("{mark} {author}{others} · {status}");
    let mark_width = if marked { 2 } else { 0 };
    let room = width.saturating_sub(3 + width_of(&head) + mark_width + pending.len() + 3);
    let mut spans = vec![bar(), Span::styled(head, t.accent)];
    if marked {
        spans.push(Span::styled(" ◆", t.warn));
    }
    if !pending.is_empty() {
        spans.push(Span::styled(pending, t.warn));
    }
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
        let mut head = vec![
            bar(),
            Span::styled(
                c.author.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(ago, t.dim),
        ];
        if c.pending {
            head.push(Span::styled(" · pending", t.warn));
        }
        out.push(Line::from(head));
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
    fn every_drawn_line_says_what_a_comment_on_it_takes() {
        let t = Theme::terminal();
        let thread = Thread {
            id: "T1".into(),
            path: "a.rs".into(),
            line: Some(1),
            start_line: None,
            side: Side::Right,
            resolved: false,
            outdated: false,
            hunk: String::new(),
            comments: vec![],
            more: 0,
            can_reply: true,
            can_resolve: true,
        };
        let opened = std::collections::HashSet::new();
        let shape = |layout| {
            let d = lines(
                "@@ -1,2 +1,2 @@\n-a\n+b\n c",
                layout,
                &[&thread],
                &opened,
                &std::collections::BTreeSet::new(),
                80,
                0,
                &t,
                0,
            );
            assert_eq!(d.spots.len(), d.lines.len(), "one spot a line");
            d.spots
                .iter()
                .map(|s| match s {
                    Spot::Hunk(n) => format!("hunk {n}"),
                    Spot::Line {
                        side, number, mark, ..
                    } => format!("{side:?} {number}{mark}"),
                    Spot::Thread { id, .. } => id.clone(),
                    Spot::Other { .. } => "other".into(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shape(DiffLayout::Unified),
            ["hunk 0", "Left 1-", "Right 1+", "T1", "Right 2 "]
        );
        assert_eq!(
            shape(DiffLayout::Split),
            ["hunk 0", "Right 1+", "T1", "Right 2 "],
            "a deleted line beside an added one: the new side"
        );
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
