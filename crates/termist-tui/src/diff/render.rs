//! A file's hunks as the rows the diff panel draws, unified or split, with the pull
//! request's line threads under the lines they are on. Pure: `view` colours them.
use super::words;
use std::ops::Range;
use termist_core::config::DiffLayout;
use termist_core::diff::{DiffLine, Hunk, LineKind};
use termist_core::github::{Side, Thread};

/// A line and the byte ranges of its text that changed against its pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub line: DiffLine,
    pub words: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// `@@ -a,b +c,d @@ context`.
    Hunk(String),
    Unified(Cell),
    /// Old on the left, new on the right; a side with nothing is empty.
    Split {
        left: Option<Cell>,
        right: Option<Cell>,
    },
    /// A thread, by its place in the threads given, under the row before it.
    Thread(usize),
    /// The heading of the threads whose line is not in this diff.
    Outdated,
}

impl Row {
    /// Whether a thread on `side`'s line `n` belongs under this row.
    fn holds(&self, side: Side, n: u32) -> bool {
        let old = |c: &Cell| c.line.old == Some(n);
        let new = |c: &Cell| c.line.new == Some(n);
        match (self, side) {
            (Row::Unified(c), Side::Left) => old(c),
            (Row::Unified(c), Side::Right) => new(c),
            (Row::Split { left, .. }, Side::Left) => left.as_ref().is_some_and(old),
            (Row::Split { right, .. }, Side::Right) => right.as_ref().is_some_and(new),
            _ => false,
        }
    }
}

/// The lines of a hunk with their changed words: each deleted line of a run is paired
/// with the added line in the same place of the run that follows it.
fn marked(hunk: &Hunk) -> Vec<Cell> {
    let lines = &hunk.lines;
    let mut cells: Vec<Cell> = lines
        .iter()
        .map(|l| Cell {
            line: l.clone(),
            words: vec![],
        })
        .collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Del {
            i += 1;
            continue;
        }
        let dels = i;
        while i < lines.len() && lines[i].kind == LineKind::Del {
            i += 1;
        }
        let adds = i;
        while i < lines.len() && lines[i].kind == LineKind::Add {
            i += 1;
        }
        for k in 0..(adds - dels).min(i - adds) {
            let (old, new) = words::changed(&lines[dels + k].text, &lines[adds + k].text);
            cells[dels + k].words = old;
            cells[adds + k].words = new;
        }
    }
    cells
}

/// One hunk as split rows: context on both sides, a run of deletions beside the run of
/// additions that follows it.
fn split(cells: Vec<Cell>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut dels: Vec<Cell> = Vec::new();
    let mut adds: Vec<Cell> = Vec::new();
    let flush = |dels: &mut Vec<Cell>, adds: &mut Vec<Cell>, rows: &mut Vec<Row>| {
        let n = dels.len().max(adds.len());
        let (mut d, mut a) = (dels.drain(..), adds.drain(..));
        for _ in 0..n {
            rows.push(Row::Split {
                left: d.next(),
                right: a.next(),
            });
        }
    };
    // What a `\ No newline` is about: the line before it.
    let mut last = LineKind::Context;
    for cell in cells {
        let kind = cell.line.kind;
        match kind {
            LineKind::Del => {
                if !adds.is_empty() {
                    flush(&mut dels, &mut adds, &mut rows);
                }
                dels.push(cell);
            }
            LineKind::Add => adds.push(cell),
            LineKind::NoNewline if last == LineKind::Del => dels.push(cell),
            LineKind::NoNewline if last == LineKind::Add => adds.push(cell),
            LineKind::NoNewline | LineKind::Context => {
                flush(&mut dels, &mut adds, &mut rows);
                rows.push(Row::Split {
                    left: Some(cell.clone()),
                    right: Some(cell),
                });
            }
        }
        if kind != LineKind::NoNewline {
            last = kind;
        }
    }
    flush(&mut dels, &mut adds, &mut rows);
    rows
}

/// The rows of a file: each hunk's header and lines, each thread under its line, and
/// the threads with no line here at the end under `Outdated`.
pub fn rows(hunks: &[Hunk], layout: DiffLayout, threads: &[&Thread]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut placed = vec![false; threads.len()];
    for hunk in hunks {
        out.push(Row::Hunk(hunk.header.clone()));
        let cells = marked(hunk);
        let lines: Vec<Row> = match layout {
            DiffLayout::Unified => cells.into_iter().map(Row::Unified).collect(),
            DiffLayout::Split => split(cells),
        };
        for row in lines {
            let under: Vec<usize> = threads
                .iter()
                .enumerate()
                .filter(|(i, t)| {
                    !placed[*i] && !t.outdated && t.line.is_some_and(|n| row.holds(t.side, n))
                })
                .map(|(i, _)| i)
                .collect();
            out.push(row);
            for i in under {
                placed[i] = true;
                out.push(Row::Thread(i));
            }
        }
    }
    if placed.iter().any(|p| !p) {
        out.push(Row::Outdated);
        out.extend((0..threads.len()).filter(|i| !placed[*i]).map(Row::Thread));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::diff::parse_patch;

    fn thread(id: &str, line: Option<u32>, side: Side, outdated: bool) -> Thread {
        Thread {
            id: id.into(),
            path: "a.ts".into(),
            line,
            side,
            resolved: false,
            outdated,
            hunk: String::new(),
            comments: vec![],
            more: 0,
            start_line: None,
            can_reply: true,
            can_resolve: true,
        }
    }

    const PATCH: &str = "@@ -38,4 +38,5 @@ export function DealerFilter\n   const dealers = useDealers();\n-  const label = 'All';\n+  const label = sel ?? 'All';\n+  useEffect(() => fetchAll(), []);\n   return (\n";

    /// Rows as text: `u` unified with numbers and mark, `s` split pairs, `T` threads.
    fn shown(rows: &[Row]) -> Vec<String> {
        let cell = |c: &Option<Cell>, right: bool| match c {
            Some(c) => {
                let n = if right { c.line.new } else { c.line.old };
                let mark = match c.line.kind {
                    LineKind::Add => "+",
                    LineKind::Del => "-",
                    LineKind::Context => " ",
                    LineKind::NoNewline => "\\",
                };
                format!("{}{mark}", n.map_or("·".into(), |n| n.to_string()))
            }
            None => "_".into(),
        };
        rows.iter()
            .map(|r| match r {
                Row::Hunk(h) => h.split(" @@").next().unwrap().to_string(),
                Row::Unified(c) => format!("u {:?} {:?} {:?}", c.line.old, c.line.new, c.line.kind),
                Row::Split { left, right } => {
                    format!("s {} | {}", cell(left, false), cell(right, true))
                }
                Row::Thread(i) => format!("T{i}"),
                Row::Outdated => "outdated".into(),
            })
            .collect()
    }

    #[test]
    fn unified_rows_keep_every_line_with_its_changed_words() {
        let hunks = parse_patch(PATCH);
        let rows = rows(&hunks, DiffLayout::Unified, &[]);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0], Row::Hunk(hunks[0].header.clone()));
        let Row::Unified(added) = &rows[3] else {
            panic!()
        };
        assert_eq!(
            added
                .words
                .iter()
                .map(|r| &added.line.text[r.clone()])
                .collect::<Vec<_>>(),
            ["sel ?? "]
        );
        let Row::Unified(second) = &rows[4] else {
            panic!()
        };
        assert!(second.words.is_empty(), "an added line with no pair");
    }

    #[test]
    fn split_rows_pair_deletions_with_the_additions_after_them() {
        let rows = rows(&parse_patch(PATCH), DiffLayout::Split, &[]);
        assert_eq!(
            shown(&rows),
            [
                "@@ -38,4 +38,5",
                "s 38  | 38 ",
                "s 39- | 39+",
                "s _ | 40+",
                "s 40  | 41 ",
            ]
        );
        let more = parse_patch("@@ -1,3 +1,1 @@\n-a\n-b\n-c\n+x\n");
        assert_eq!(
            shown(&super::rows(&more, DiffLayout::Split, &[])),
            ["@@ -1,3 +1,1", "s 1- | 1+", "s 2- | _", "s 3- | _"]
        );
    }

    #[test]
    fn no_newline_stays_on_its_side() {
        let p = parse_patch(
            "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file",
        );
        assert_eq!(
            shown(&rows(&p, DiffLayout::Split, &[])),
            ["@@ -1 +1", "s 1- | 1+", "s ·\\ | ·\\"]
        );
    }

    #[test]
    fn threads_sit_under_their_line_on_their_side_and_the_rest_at_the_end() {
        let hunks = parse_patch(PATCH);
        let right = thread("R", Some(40), Side::Right, false);
        let left = thread("L", Some(39), Side::Left, false);
        let gone = thread("O", Some(40), Side::Right, true);
        let far = thread("F", Some(900), Side::Right, false);
        let threads = [&right, &left, &gone, &far];
        assert_eq!(
            shown(&rows(&hunks, DiffLayout::Unified, &threads)),
            [
                "@@ -38,4 +38,5",
                "u Some(38) Some(38) Context",
                "u Some(39) None Del",
                "T1",
                "u None Some(39) Add",
                "u None Some(40) Add",
                "T0",
                "u Some(40) Some(41) Context",
                "outdated",
                "T2",
                "T3",
            ]
        );
        assert_eq!(
            shown(&rows(&hunks, DiffLayout::Split, &threads))[2..5],
            ["s 39- | 39+", "T1", "s _ | 40+"]
        );
    }
}
