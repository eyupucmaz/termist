//! The files of a diff as a tree: folders first, a chain of lone folders on one row
//! (`src/search/`), folds, and the `/` search over paths.
use crate::list_picker::matches;
use std::collections::{BTreeMap, HashSet};
use termist_core::github::DiffFile;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// `path` is the folder's whole path (`src/search`), the key of its fold.
    Dir {
        path: String,
        label: String,
        folded: bool,
    },
    /// `index` is the file's place in the diff's list.
    File { index: usize, label: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub node: Node,
}

impl TreeRow {
    pub fn file(&self) -> Option<usize> {
        match self.node {
            Node::File { index, .. } => Some(index),
            Node::Dir { .. } => None,
        }
    }
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

/// Case only breaks ties: `api` and `Api` sit together.
fn by_name(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b))
}

fn walk(dir: &Dir, prefix: &str, depth: usize, folded: &HashSet<String>, out: &mut Vec<TreeRow>) {
    let mut names: Vec<&String> = dir.dirs.keys().collect();
    names.sort_by(|a, b| by_name(a, b));
    for name in names {
        let mut sub = &dir.dirs[name];
        let mut path = format!("{prefix}{name}");
        let mut label = name.clone();
        while sub.files.is_empty() && sub.dirs.len() == 1 {
            let (next, below) = sub.dirs.iter().next().expect("one folder");
            path = format!("{path}/{next}");
            label = format!("{label}/{next}");
            sub = below;
        }
        let is_folded = folded.contains(&path);
        out.push(TreeRow {
            depth,
            node: Node::Dir {
                path: path.clone(),
                label: format!("{label}/"),
                folded: is_folded,
            },
        });
        if !is_folded {
            walk(sub, &format!("{path}/"), depth + 1, folded, out);
        }
    }
    let mut files = dir.files.clone();
    files.sort_by(|a, b| by_name(&a.0, &b.0));
    for (label, index) in files {
        out.push(TreeRow {
            depth,
            node: Node::File { index, label },
        });
    }
}

/// The rows of the tree. With a `query`, only the files whose path matches it, every
/// folder open.
pub fn rows(files: &[DiffFile], folded: &HashSet<String>, query: &str) -> Vec<TreeRow> {
    let mut root = Dir::default();
    for (index, f) in files.iter().enumerate() {
        if !query.is_empty() && !matches(query, &f.path) {
            continue;
        }
        let mut parts: Vec<&str> = f.path.split('/').filter(|p| !p.is_empty()).collect();
        let Some(name) = parts.pop() else {
            continue;
        };
        let mut dir = &mut root;
        for part in parts {
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
        dir.files.push((name.to_string(), index));
    }
    let none = HashSet::new();
    let folded = if query.is_empty() { folded } else { &none };
    let mut out = Vec::new();
    walk(&root, "", 0, folded, &mut out);
    out
}

/// The files in the order the tree shows them, folds or not: `J` and `K` go by it.
pub fn order(files: &[DiffFile]) -> Vec<usize> {
    rows(files, &HashSet::new(), "")
        .iter()
        .filter_map(TreeRow::file)
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use termist_core::github::{Patch, Viewed};

    pub fn file(path: &str) -> DiffFile {
        DiffFile {
            path: path.into(),
            previous: None,
            change: 'M',
            additions: 1,
            deletions: 1,
            viewed: Viewed::Unviewed,
            patch: Patch::Text("@@ -1 +1 @@\n-a\n+b".into()),
            url: format!("https://github.com/acme/site/pull/212/files#diff-{path}"),
        }
    }

    fn shown(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let label = match &r.node {
                    Node::Dir { label, folded, .. } => {
                        format!("{}{label}", if *folded { "▸" } else { "▾" })
                    }
                    Node::File { label, .. } => label.clone(),
                };
                format!("{}{label}", "  ".repeat(r.depth))
            })
            .collect()
    }

    fn files() -> Vec<DiffFile> {
        [
            "package.json",
            "src/search/DealerFilter.tsx",
            "src/search/useDealers.ts",
            "src/api/client.ts",
            "src/App.tsx",
            "README.md",
        ]
        .into_iter()
        .map(file)
        .collect()
    }

    #[test]
    fn folders_come_first_and_lone_folders_join() {
        let rows = rows(&files(), &HashSet::new(), "");
        assert_eq!(
            shown(&rows),
            [
                "▾src/",
                "  ▾api/",
                "    client.ts",
                "  ▾search/",
                "    DealerFilter.tsx",
                "    useDealers.ts",
                "  App.tsx",
                "package.json",
                "README.md",
            ]
        );
        let lone = super::rows(&[file("a/b/c/x.rs")], &HashSet::new(), "");
        assert_eq!(shown(&lone), ["▾a/b/c/", "  x.rs"]);
        assert_eq!(
            lone[0].node,
            Node::Dir {
                path: "a/b/c".into(),
                label: "a/b/c/".into(),
                folded: false
            }
        );
    }

    #[test]
    fn a_folded_folder_hides_what_is_in_it() {
        let folded: HashSet<String> = ["src/search".to_string()].into();
        assert_eq!(
            shown(&rows(&files(), &folded, "")),
            [
                "▾src/",
                "  ▾api/",
                "    client.ts",
                "  ▸search/",
                "  App.tsx",
                "package.json",
                "README.md",
            ]
        );
    }

    #[test]
    fn the_search_keeps_matching_files_and_opens_their_folders() {
        let folded: HashSet<String> = ["src/search".to_string()].into();
        assert_eq!(
            shown(&rows(&files(), &folded, "dealer")),
            ["▾src/search/", "  DealerFilter.tsx", "  useDealers.ts"]
        );
        assert!(rows(&files(), &folded, "nothing").is_empty());
    }

    #[test]
    fn order_follows_the_tree_and_ignores_folds() {
        let f = files();
        let order: Vec<&str> = order(&f).iter().map(|i| f[*i].path.as_str()).collect();
        assert_eq!(
            order,
            [
                "src/api/client.ts",
                "src/search/DealerFilter.tsx",
                "src/search/useDealers.ts",
                "src/App.tsx",
                "package.json",
                "README.md",
            ]
        );
    }
}
