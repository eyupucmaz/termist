//! Unified diffs as GitHub sends them per file (`patch`) and `git diff` prints them:
//! hunks of numbered lines. Pure, so the PR diff and a local diff read the same way.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    /// `\ No newline at end of file`, about the line before it.
    NoNewline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the old file; `None` for an added line.
    pub old: Option<u32>,
    /// The line's number in the new file; `None` for a deleted line.
    pub new: Option<u32>,
    /// Without the leading mark.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The whole `@@ -a,b +c,d @@ context` line.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

/// `-a,b` or `+c` → `a`; the count is not needed, the lines say how many there are.
fn start(part: &str, sign: char) -> Option<u32> {
    part.strip_prefix(sign)?.split(',').next()?.parse().ok()
}

/// The two starts of a hunk header, `None` when it is not one.
fn header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let mut parts = rest.split(' ');
    let old = start(parts.next()?, '-')?;
    let new = start(parts.next()?, '+')?;
    Some((old, new))
}

/// The hunks of `patch`. Lines before the first hunk (a `diff --git` head) and lines a
/// hunk cannot hold are skipped; nothing panics on odd input.
pub fn parse_patch(patch: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    // The newline that ends the last line is not a line of its own.
    let patch = patch.strip_suffix('\n').unwrap_or(patch);
    for raw in patch.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with("@@") {
            if let Some((o, n)) = header(line) {
                hunks.push(Hunk {
                    header: line.to_string(),
                    old_start: o,
                    new_start: n,
                    lines: vec![],
                });
                (old, new) = (o, n);
            }
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let mut chars = line.chars();
        let (kind, o, n) = match chars.next() {
            Some(' ') => (LineKind::Context, Some(old), Some(new)),
            Some('+') => (LineKind::Add, None, Some(new)),
            Some('-') => (LineKind::Del, Some(old), None),
            Some('\\') => (LineKind::NoNewline, None, None),
            // Some tools drop the space of an empty context line.
            None => (LineKind::Context, Some(old), Some(new)),
            Some(_) => continue,
        };
        let text = match kind {
            LineKind::NoNewline => line.trim_start_matches('\\').trim_start().to_string(),
            _ => chars.as_str().to_string(),
        };
        if o.is_some() {
            old += 1;
        }
        if n.is_some() {
            new += 1;
        }
        hunk.lines.push(DiffLine {
            kind,
            old: o,
            new: n,
            text,
        });
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(h: &Hunk) -> String {
        h.lines
            .iter()
            .map(|l| match l.kind {
                LineKind::Context => ' ',
                LineKind::Add => '+',
                LineKind::Del => '-',
                LineKind::NoNewline => '\\',
            })
            .collect()
    }

    fn numbers(h: &Hunk) -> Vec<(Option<u32>, Option<u32>)> {
        h.lines.iter().map(|l| (l.old, l.new)).collect()
    }

    #[test]
    fn a_hunk_numbers_its_lines_on_both_sides() {
        let p = "@@ -38,4 +38,5 @@ export function DealerFilter\n   const a = 1;\n-  const b = 2;\n+  const b = 3;\n+  const c = 4;\n   return a;\n";
        let h = parse_patch(p);
        assert_eq!(h.len(), 1);
        assert_eq!(
            h[0].header,
            "@@ -38,4 +38,5 @@ export function DealerFilter"
        );
        assert_eq!((h[0].old_start, h[0].new_start), (38, 38));
        assert_eq!(kinds(&h[0]), " -++ ");
        assert_eq!(
            numbers(&h[0]),
            [
                (Some(38), Some(38)),
                (Some(39), None),
                (None, Some(39)),
                (None, Some(40)),
                (Some(40), Some(41)),
            ]
        );
        assert_eq!(h[0].lines[1].text, "  const b = 2;");
    }

    #[test]
    fn a_new_file_and_a_deleted_one() {
        let added = parse_patch("@@ -0,0 +1,2 @@\n+one\n+two");
        assert_eq!(numbers(&added[0]), [(None, Some(1)), (None, Some(2))]);
        let gone = parse_patch("@@ -1,2 +0,0 @@\n-one\n-two");
        assert_eq!(numbers(&gone[0]), [(Some(1), None), (Some(2), None)]);
    }

    #[test]
    fn many_hunks_and_headers_without_counts() {
        let h = parse_patch("@@ -1 +1 @@\n-a\n+b\n@@ -10,2 +10,3 @@ fn x\n c\n+d\n e");
        assert_eq!(h.len(), 2);
        assert_eq!((h[1].old_start, h[1].new_start), (10, 10));
        assert_eq!(numbers(&h[1])[2], (Some(11), Some(12)));
    }

    #[test]
    fn no_newline_at_the_end_is_its_own_line() {
        let h = parse_patch(
            "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file",
        );
        assert_eq!(kinds(&h[0]), "-\\+\\");
        assert_eq!(h[0].lines[1].text, "No newline at end of file");
        assert_eq!(numbers(&h[0])[2], (None, Some(1)));
    }

    #[test]
    fn carriage_returns_go_and_odd_lines_are_skipped() {
        let h = parse_patch("diff --git a/x b/x\r\n@@ -1,2 +1,2 @@\r\n a\r\n-b\r\n+c\r\n");
        assert_eq!(h.len(), 1);
        assert_eq!(kinds(&h[0]), " -+");
        assert_eq!(h[0].lines[0].text, "a");
        assert!(parse_patch("@@ nonsense @@\n+a").is_empty());
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("+a\n-b").is_empty(), "no hunk to hold them");
    }

    #[test]
    fn an_empty_context_line_without_its_space_still_counts() {
        let h = parse_patch("@@ -1,3 +1,3 @@\n a\n\n-b\n+c");
        assert_eq!(kinds(&h[0]), "  -+");
        assert_eq!(numbers(&h[0])[2], (Some(3), None));
    }
}
