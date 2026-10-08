//! `f` and `F`: a box to find a file of the repo, or the lines some text is on; the
//! result opens in the editor. `f` ranks the repo's files here as you type; `F` asks
//! the daemon's `git grep` a moment after you stop.
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use termist_core::GrepMatch;

/// Results drawn, at most.
pub const SHOWN: usize = 200;
/// How long after the last key `F` asks.
pub const WAIT: Duration = Duration::from_millis(150);
/// `F` asks for this many characters or more.
pub const MIN_QUERY: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindKind {
    Files,
    Grep,
}

/// A result: a file, or a line of one; `marks` are the characters the query matched
/// (of `path` for a file, of `text` for a line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: Option<u32>,
    pub text: Option<String>,
    pub marks: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finder {
    pub kind: FindKind,
    /// The selection's folder, where it was opened.
    pub folder: PathBuf,
    pub query: String,
    /// The repo's root, once the daemon said; results are relative to it.
    pub root: Option<PathBuf>,
    /// The repo's files once read, and how many past those sent.
    pub files: Option<Vec<String>>,
    pub more_files: u32,
    pub hits: Vec<Hit>,
    /// `F` found more lines than it sent.
    pub more: bool,
    pub highlight: usize,
    /// The newest ask; an answer to an older one is dropped.
    pub ticket: u64,
    /// An ask is on its way.
    pub waiting: bool,
    /// When `F` asks, after the last key.
    pub due: Option<Instant>,
    /// Why the last ask failed.
    pub failed: Option<String>,
}

impl Finder {
    pub fn new(kind: FindKind, folder: PathBuf) -> Finder {
        Finder {
            kind,
            folder,
            query: String::new(),
            root: None,
            files: None,
            more_files: 0,
            hits: vec![],
            more: false,
            highlight: 0,
            ticket: 0,
            waiting: false,
            due: None,
            failed: None,
        }
    }

    /// The repo's files came.
    pub fn set_files(&mut self, root: PathBuf, files: Vec<String>, more: u32) {
        self.root = Some(root);
        self.files = Some(files);
        self.more_files = more;
        self.waiting = false;
        self.failed = None;
        if self.kind == FindKind::Files {
            self.refilter();
        }
    }

    /// `f`'s results for the query as it is.
    fn refilter(&mut self) {
        self.hits = self
            .files
            .as_deref()
            .map(|files| rank(files, &self.query))
            .unwrap_or_default();
        self.more = false;
        self.highlight = 0;
    }

    /// `F`'s lines came.
    pub fn set_grep(&mut self, root: PathBuf, matches: Vec<GrepMatch>, more: bool) {
        self.root = Some(root);
        self.waiting = false;
        self.failed = None;
        self.more = more;
        self.highlight = 0;
        self.hits = matches
            .into_iter()
            .map(|m| Hit {
                marks: marks_of(&m.text, &self.query),
                path: m.path,
                line: Some(m.line),
                text: Some(m.text),
            })
            .collect();
    }

    /// The query changed: `f` ranks again now, `F` asks a moment later.
    pub fn typed(&mut self, now: Instant) {
        match self.kind {
            FindKind::Files => self.refilter(),
            FindKind::Grep if self.query.chars().count() >= MIN_QUERY => {
                self.due = Some(now + WAIT);
            }
            FindKind::Grep => {
                self.due = None;
                self.hits.clear();
                self.more = false;
                self.highlight = 0;
            }
        }
    }

    /// `F`'s query, when it is time to ask for it.
    pub fn due_query(&mut self, now: Instant) -> Option<String> {
        if self.due.is_none_or(|due| now < due) {
            return None;
        }
        self.due = None;
        self.waiting = true;
        Some(self.query.clone())
    }

    /// The highlighted result: its path (relative to the root) and line.
    pub fn chosen(&self) -> Option<(String, Option<u32>)> {
        let hit = self.hits.get(self.highlight)?;
        Some((hit.path.clone(), hit.line))
    }
}

/// The files `query` matches, best first (all of them, in order, for no query), at
/// most `SHOWN`, with the characters it matched.
pub fn rank(files: &[String], query: &str) -> Vec<Hit> {
    if query.trim().is_empty() {
        return files
            .iter()
            .take(SHOWN)
            .map(|path| Hit {
                path: path.clone(),
                line: None,
                text: None,
                marks: vec![],
            })
            .collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, Vec<u32>)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            let mut marks = Vec::new();
            let score = pattern.indices(Utf32Str::new(path, &mut buf), &mut matcher, &mut marks)?;
            marks.sort_unstable();
            marks.dedup();
            Some((score, i, marks))
        })
        .collect();
    // Best first; between equals the shorter path, then the list's order.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(files[a.1].len().cmp(&files[b.1].len()))
            .then(a.1.cmp(&b.1))
    });
    scored
        .into_iter()
        .take(SHOWN)
        .map(|(_, i, marks)| Hit {
            path: files[i].clone(),
            line: None,
            text: None,
            marks,
        })
        .collect()
}

/// Where `query` is in `text`: the characters of each place (any case unless the query
/// has a capital).
pub fn marks_of(text: &str, query: &str) -> Vec<u32> {
    if query.is_empty() {
        return vec![];
    }
    let any_case = !query.chars().any(char::is_uppercase);
    let fold = |c: char| {
        if any_case {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        }
    };
    let text: Vec<char> = text.chars().map(fold).collect();
    let query: Vec<char> = query.chars().map(fold).collect();
    let mut marks = Vec::new();
    let mut i = 0;
    while i + query.len() <= text.len() {
        if text[i..i + query.len()] == query[..] {
            marks.extend((i..i + query.len()).map(|c| c as u32));
            i += query.len();
        } else {
            i += 1;
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.path.as_str()).collect()
    }

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn files_are_ranked_by_how_well_they_match_and_say_where() {
        let list = files(&[
            "README.md",
            "crates/termist-tui/src/app.rs",
            "crates/termist-daemon/src/registry.rs",
            "docs/app-notes.md",
        ]);
        let hits = rank(&list, "app");
        let mut best = names(&hits)[..2].to_vec();
        best.sort();
        assert_eq!(
            best,
            ["crates/termist-tui/src/app.rs", "docs/app-notes.md"],
            "where app starts a word"
        );
        assert!(!names(&hits).contains(&"README.md"), "no a, p, p in order");
        let app = hits.iter().find(|h| h.path.ends_with("app.rs")).unwrap();
        let marked: String = app
            .marks
            .iter()
            .map(|i| app.path.chars().nth(*i as usize).unwrap())
            .collect();
        assert_eq!(marked, "app");
        assert_eq!(
            names(&rank(&list, "")),
            names(
                &list
                    .iter()
                    .map(|p| Hit {
                        path: p.clone(),
                        line: None,
                        text: None,
                        marks: vec![],
                    })
                    .collect::<Vec<_>>()
            ),
            "no query: all, as they are"
        );
        assert_eq!(
            names(&rank(&list, "rgstry")),
            ["crates/termist-daemon/src/registry.rs"]
        );
        let many: Vec<String> = (0..SHOWN + 5).map(|n| format!("f{n}.rs")).collect();
        assert_eq!(rank(&many, "f").len(), SHOWN);
    }

    #[test]
    fn a_line_marks_where_the_text_is() {
        assert_eq!(
            marks_of("let Redirect = redirect;", "redirect"),
            [4, 5, 6, 7, 8, 9, 10, 11, 15, 16, 17, 18, 19, 20, 21, 22]
        );
        assert_eq!(
            marks_of("let Redirect = redirect;", "Redirect"),
            [4, 5, 6, 7, 8, 9, 10, 11]
        );
        assert!(marks_of("abc", "").is_empty());
    }

    #[test]
    fn f_ranks_as_you_type_and_f_shift_asks_a_moment_after_the_last_key() {
        let now = Instant::now();
        let mut f = Finder::new(FindKind::Files, "/w/site/src".into());
        f.set_files("/w/site".into(), files(&["src/auth.rs", "src/login.rs"]), 0);
        assert_eq!(f.hits.len(), 2);
        f.query = "login".into();
        f.typed(now);
        assert_eq!(names(&f.hits), ["src/login.rs"]);
        assert_eq!(f.chosen(), Some(("src/login.rs".into(), None)));
        let mut g = Finder::new(FindKind::Grep, "/w/site".into());
        g.query = "r".into();
        g.typed(now);
        assert_eq!(g.due, None, "one character asks nothing");
        g.query = "re".into();
        g.typed(now);
        assert_eq!(g.due_query(now), None, "not yet");
        assert_eq!(g.due_query(now + WAIT), Some("re".into()));
        assert_eq!(g.due_query(now + WAIT * 2), None, "asked once");
        assert!(g.waiting);
        g.set_grep(
            "/w/site".into(),
            vec![GrepMatch {
                path: "src/auth.rs".into(),
                line: 42,
                text: "let redirect = q;".into(),
            }],
            false,
        );
        assert!(!g.waiting);
        assert_eq!(g.hits[0].marks, [4, 5, 8, 9], "re-di-re-ct");
        assert_eq!(g.chosen(), Some(("src/auth.rs".into(), Some(42))));
    }
}
