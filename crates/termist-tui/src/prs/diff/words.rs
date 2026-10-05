//! The words that changed between a deleted line and the added line paired with it.
//! A plain longest-common-subsequence over words, kept cheap: lines past a few hundred
//! characters are not compared.
use std::ops::Range;

/// Lines longer than this, in bytes, are not compared.
pub const MAX_BYTES: usize = 500;
/// Nor lines of more words than this.
pub const MAX_WORDS: usize = 200;

/// `s` in words, as byte ranges: a run of letters, digits and `_`, a run of spaces, or
/// any other single character.
pub fn split(s: &str) -> Vec<Range<usize>> {
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last = None;
    for (i, c) in s.char_indices() {
        let k = kind(c);
        match out.last_mut() {
            Some(r) if last == Some(k) && k != 2 => r.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

/// Neighbouring ranges as one.
fn merge(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if last.end == r.start => last.end = r.end,
            _ => out.push(r),
        }
    }
    out
}

/// The byte ranges that changed in `old` and in `new`. Nothing when a line is too long
/// to compare, or when the two have no word in common: then the whole line is the
/// change, and its colour already says so.
pub fn changed(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old.len() > MAX_BYTES || new.len() > MAX_BYTES {
        return (vec![], vec![]);
    }
    let (a, b) = (split(old), split(new));
    if a.len() > MAX_WORDS || b.len() > MAX_WORDS {
        return (vec![], vec![]);
    }
    let word = |s: &str, r: &Range<usize>| s[r.clone()].to_string();
    let (aw, bw): (Vec<String>, Vec<String>) = (
        a.iter().map(|r| word(old, r)).collect(),
        b.iter().map(|r| word(new, r)).collect(),
    );
    // lcs[i][j]: the longest common run of aw[i..] and bw[j..].
    let mut lcs = vec![vec![0u16; bw.len() + 1]; aw.len() + 1];
    for i in (0..aw.len()).rev() {
        for j in (0..bw.len()).rev() {
            lcs[i][j] = if aw[i] == bw[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; aw.len()], vec![false; bw.len()]);
    let (mut i, mut j) = (0, 0);
    while i < aw.len() && j < bw.len() {
        if aw[i] == bw[j] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let common = aw
        .iter()
        .zip(&keep_a)
        .any(|(w, k)| *k && !w.trim().is_empty());
    if !common {
        return (vec![], vec![]);
    }
    let gone = |ranges: &[Range<usize>], keep: &[bool]| {
        merge(
            ranges
                .iter()
                .zip(keep)
                .filter(|(_, k)| !**k)
                .map(|(r, _)| r.clone())
                .collect(),
        )
    };
    (gone(&a, &keep_a), gone(&b, &keep_b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<&str> {
        split(s).into_iter().map(|r| &s[r]).collect()
    }

    fn texts<'a>(s: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &s[r.clone()]).collect()
    }

    #[test]
    fn words_are_runs_of_letters_or_spaces_and_single_marks() {
        assert_eq!(
            words("  let x_1 = f(a, b);"),
            [
                "  ", "let", " ", "x_1", " ", "=", " ", "f", "(", "a", ",", " ", "b", ")", ";"
            ]
        );
        assert_eq!(words("çiçek→ağaç"), ["çiçek", "→", "ağaç"]);
    }

    #[test]
    fn only_the_changed_words_are_marked() {
        let old = "  const label = 'All';";
        let new = "  const label = sel ?? 'All';";
        let (o, n) = changed(old, new);
        assert!(o.is_empty());
        assert_eq!(texts(new, &n), ["sel ?? "]);
        let (o, n) = changed("fn a(x: u8)", "fn b(x: u16)");
        assert_eq!(texts("fn a(x: u8)", &o), ["a", "u8"]);
        assert_eq!(texts("fn b(x: u16)", &n), ["b", "u16"]);
    }

    #[test]
    fn lines_with_nothing_in_common_or_too_long_mark_nothing() {
        assert_eq!(changed("alpha beta", "gamma delta"), (vec![], vec![]));
        let long = "x ".repeat(300);
        assert_eq!(changed(&long, "x"), (vec![], vec![]));
        let many = "a,".repeat(150);
        assert_eq!(changed(&many, "a"), (vec![], vec![]));
    }
}
