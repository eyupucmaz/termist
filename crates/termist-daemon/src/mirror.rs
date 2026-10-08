//! Ayna: a folder's diff as `g` shows it, the whole branch since it left its base or
//! only what is not committed, with new files too; whether it changed since it was
//! read; and which of its files are still as they were when marked reviewed.
use crate::github::worktree::Git;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use termist_core::github::{DiffFile, Patch, Viewed};
use termist_core::{DiffMode, LocalDiffData};

/// Files past this many are counted, not read (as a pull request's diff).
pub const MAX_FILES: usize = 300;
/// Patches past this many bytes, all files together, are not kept.
pub const MAX_BYTES: usize = crate::github::files::MAX_BYTES;
/// A new file past this size is not read.
pub const NEW_FILE_LIMIT: u64 = 1 << 20;

/// The files of `git diff` output (run with `core.quotePath=false`), each with its
/// patch from its first hunk on. Odd lines are skipped; nothing panics.
pub fn split(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<File> = Vec::new();
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(File::new(rest));
            continue;
        }
        let Some(f) = files.last_mut() else {
            continue;
        };
        if f.hunks {
            f.patch.push(line);
            continue;
        }
        if line.starts_with("@@") {
            f.hunks = true;
            f.patch.push(line);
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if let Some(p) = path_of(p, "b/") {
                f.path = p;
            }
        } else if let Some(p) = line.strip_prefix("--- ") {
            if let Some(p) = path_of(p, "a/") {
                f.old = Some(p);
            }
        } else if line.starts_with("new file mode") {
            f.change = 'A';
        } else if line.starts_with("deleted file mode") {
            f.change = 'D';
        } else if let Some(p) = line.strip_prefix("rename from ") {
            (f.change, f.old) = ('R', Some(p.to_string()));
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.path = p.to_string();
        } else if let Some(p) = line.strip_prefix("copy from ") {
            (f.change, f.old) = ('C', Some(p.to_string()));
        } else if let Some(p) = line.strip_prefix("copy to ") {
            f.path = p.to_string();
        } else if line.starts_with("Binary files ") {
            f.binary = true;
        }
    }
    files.into_iter().map(File::done).collect()
}

/// A file of the diff while it is read.
struct File<'a> {
    path: String,
    old: Option<String>,
    change: char,
    binary: bool,
    hunks: bool,
    patch: Vec<&'a str>,
}

impl<'a> File<'a> {
    /// From `a/x b/x`: the path when it is the same on both sides (the usual case, and
    /// one with spaces); a renamed one is named again by its `rename` lines.
    fn new(names: &str) -> File<'a> {
        let half = names.len().saturating_sub(1) / 2;
        let path = match (names.get(..half), names.get(half + 1..)) {
            (Some(a), Some(b))
                if a.strip_prefix("a/").is_some() && Some(&a[2..]) == b.strip_prefix("b/") =>
            {
                b[2..].to_string()
            }
            _ => names
                .rsplit_once(" b/")
                .map_or(names, |(_, b)| b)
                .to_string(),
        };
        File {
            path,
            old: None,
            change: 'M',
            binary: false,
            hunks: false,
            patch: vec![],
        }
    }

    fn done(self) -> DiffFile {
        // The newline that ends the output is not a line of the last file.
        let mut lines = self.patch;
        while lines.last() == Some(&"") {
            lines.pop();
        }
        let (mut additions, mut deletions) = (0, 0);
        for l in &lines {
            match l.as_bytes().first() {
                Some(b'+') => additions += 1,
                Some(b'-') => deletions += 1,
                _ => {}
            }
        }
        let patch = if self.binary {
            Patch::Binary
        } else if lines.is_empty() && self.change == 'R' {
            Patch::Renamed
        } else {
            Patch::Text(lines.join("\n"))
        };
        // A deleted file is named by its old side; a moved one keeps where it came from.
        let previous = self.old.filter(|_| matches!(self.change, 'R' | 'C'));
        DiffFile {
            path: self.path,
            previous,
            change: self.change,
            additions,
            deletions,
            viewed: Viewed::Unviewed,
            patch,
            url: String::new(),
        }
    }
}

/// `a/src/x.rs` (or `b/…`) → `src/x.rs`; `/dev/null` → `None`. git ends a name with
/// spaces with a tab.
fn path_of(text: &str, side: &str) -> Option<String> {
    let text = text.strip_suffix('\t').unwrap_or(text);
    text.strip_prefix(side).map(str::to_string)
}

/// What `read` found: the diff, the folder's root, and each file's content hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub root: PathBuf,
    pub data: LocalDiffData,
    pub hashes: HashMap<String, String>,
}

/// The diff of the repo the folder at `path` is in. `base` names the ref the branch is
/// measured from (a worktree termist made); else the default branch's `origin/` one,
/// or the local one. `git` must give its output as it is (a diff ends in spaces).
pub fn read(path: &Path, mode: DiffMode, base: Option<&str>, git: Git) -> Result<Read, String> {
    if !path.is_dir() {
        return Err("the folder is gone".into());
    }
    let top = git(path, &["rev-parse", "--show-toplevel"])
        .map_err(|_| "not a git repository".to_string())?;
    let root = crate::place::resolved(Path::new(top.trim()));
    let head = match git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Ok(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => git(&root, &["rev-parse", "--short", "HEAD"])
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|_| "HEAD".into()),
    };
    let (since, label) = match mode {
        DiffMode::Uncommitted => ("HEAD".to_string(), "HEAD".to_string()),
        DiffMode::Branch => {
            let against = base
                .map(str::to_string)
                .unwrap_or_else(|| crate::worktrees::base_ref(&root, git));
            match git(&root, &["merge-base", "HEAD", &against]).map(|m| m.trim().to_string()) {
                Ok(m) if !m.is_empty() => (m, against),
                _ => (
                    "HEAD".to_string(),
                    format!("HEAD (no merge base with {against})"),
                ),
            }
        }
    };
    let out = git(
        &root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-M",
            &since,
        ],
    )?;
    // Known files from the diff, new ones by name: only those kept are read.
    let others = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut all: Vec<(String, Option<DiffFile>)> = split(&out)
        .into_iter()
        .map(|f| (f.path.clone(), Some(f)))
        .chain(
            others
                .split('\0')
                .filter(|n| !n.is_empty())
                .map(|n| (n.to_string(), None)),
        )
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let more = all.len().saturating_sub(MAX_FILES) as u32;
    all.truncate(MAX_FILES);
    let mut files: Vec<DiffFile> = all
        .into_iter()
        .map(|(name, known)| known.unwrap_or_else(|| new_file(&root, &name)))
        .collect();
    let mut bytes = 0;
    for f in &mut files {
        if let Patch::Text(t) = &f.patch {
            bytes += t.len();
            if bytes > MAX_BYTES {
                f.patch = Patch::TooLarge;
            }
        }
    }
    let dirty = git(&root, &["--no-optional-locks", "status", "--porcelain"])
        .is_ok_and(|s| !s.trim().is_empty());
    let hashes = files
        .iter()
        .map(|f| (f.path.clone(), content_hash(&root, &f.path)))
        .collect();
    Ok(Read {
        data: LocalDiffData {
            head,
            base: label,
            dirty,
            files,
            more,
        },
        root,
        hashes,
    })
}

#[cfg(test)]
thread_local! {
    /// How many new files `new_file` read, for the tests.
    static NEW_FILES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A file git does not know yet, as a diff that adds every line of it.
fn new_file(root: &Path, name: &str) -> DiffFile {
    #[cfg(test)]
    NEW_FILES_READ.with(|n| n.set(n.get() + 1));
    let path = root.join(name);
    let mut file = DiffFile {
        path: name.to_string(),
        previous: None,
        change: 'A',
        additions: 0,
        deletions: 0,
        viewed: Viewed::Unviewed,
        patch: Patch::Text(String::new()),
        url: String::new(),
    };
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    if size > NEW_FILE_LIMIT {
        file.patch = Patch::TooLarge;
        return file;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return file;
    };
    if bytes.contains(&0) {
        file.patch = Patch::Binary;
        return file;
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    file.additions = lines.len() as u32;
    if !lines.is_empty() {
        let count = match lines.len() {
            1 => "1".to_string(),
            n => format!("1,{n}"),
        };
        let body: Vec<String> = lines.iter().map(|l| format!("+{l}")).collect();
        file.patch = Patch::Text(format!("@@ -0,0 +{count} @@\n{}", body.join("\n")));
    }
    file
}

/// Files past this size are told apart by size and time, not read whole.
const HASH_LIMIT: u64 = 16 << 20;

/// A file's content as it is now, to tell whether it changed since it was marked:
/// `deleted` when it is not there.
pub fn content_hash(root: &Path, file: &str) -> String {
    use sha2::{Digest, Sha256};
    let path = root.join(file);
    let Ok(meta) = std::fs::metadata(&path) else {
        return "deleted".into();
    };
    let digest = if meta.len() > HASH_LIMIT {
        let when = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        Sha256::digest(format!("{}:{when}", meta.len()))
    } else {
        match std::fs::read(&path) {
            Ok(bytes) => Sha256::digest(bytes),
            Err(_) => return "deleted".into(),
        }
    };
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sets each file's `viewed` from the marks kept for its folder: reviewed when its
/// content is what it was when marked, dismissed when it changed since.
pub fn mark(
    data: &mut LocalDiffData,
    hashes: &HashMap<String, String>,
    marks: &HashMap<String, String>,
) {
    for f in &mut data.files {
        f.viewed = match (marks.get(&f.path), hashes.get(&f.path)) {
            (None, _) => Viewed::Unviewed,
            (Some(kept), Some(now)) if kept == now => Viewed::Viewed,
            (Some(_), _) => Viewed::Dismissed,
        };
    }
}

/// How many of the marked files are still as they were when marked (a deleted one
/// marked deleted counts too).
pub fn count_reviewed(root: &Path, marks: &HashMap<String, String>) -> u32 {
    marks
        .iter()
        .filter(|(file, hash)| content_hash(root, file) == **hash)
        .count() as u32
}

/// Changes when anything `read` would see changes: the commit, what git calls changed,
/// and the size and time of each changed file (a changed file changing again leaves
/// git's status as it was). `None` when git cannot say.
pub fn fingerprint(root: &Path, git: Git) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let status = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )
    .ok()?;
    let head = git(root, &["rev-parse", "-q", "--verify", "HEAD"]).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    head.hash(&mut h);
    status.hash(&mut h);
    for entry in status.split('\0').filter(|e| !e.is_empty()) {
        // `XY name`; a renamed entry is followed by where it came from, alone.
        let name = match entry.as_bytes().get(2) {
            Some(b' ') if entry.len() > 3 => &entry[3..],
            _ => entry,
        };
        if let Ok(m) = std::fs::metadata(root.join(name)) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    Some(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(f: &DiffFile) -> &str {
        match &f.patch {
            Patch::Text(t) => t,
            other => panic!("{} has no text: {other:?}", f.path),
        }
    }

    #[test]
    fn a_diff_splits_into_its_files_with_what_each_changed() {
        let out = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 one
-two
+deux
+trois
 
diff --git a/new.md b/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.md
@@ -0,0 +1 @@
+hello
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 4444444..0000000
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
diff --git a/lib/x.rs b/lib/y.rs
similarity index 90%
rename from lib/x.rs
rename to lib/y.rs
index 5555555..6666666 100644
--- a/lib/x.rs
+++ b/lib/y.rs
@@ -1 +1 @@
-x
+y
diff --git a/same.rs b/moved.rs
similarity index 100%
rename from same.rs
rename to moved.rs
diff --git a/logo.png b/logo.png
index 7777777..8888888 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
diff --git a/with space.txt b/with space.txt
index 9999999..aaaaaaa 100644
--- a/with space.txt\t
+++ b/with space.txt\t
@@ -1 +1 @@
-a
+b
";
        let files = split(out);
        let sum: Vec<(&str, Option<&str>, char, u32, u32)> = files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.previous.as_deref(),
                    f.change,
                    f.additions,
                    f.deletions,
                )
            })
            .collect();
        assert_eq!(
            sum,
            [
                ("src/a.rs", None, 'M', 2, 1),
                ("new.md", None, 'A', 1, 0),
                ("old.txt", None, 'D', 0, 2),
                ("lib/y.rs", Some("lib/x.rs"), 'R', 1, 1),
                ("moved.rs", Some("same.rs"), 'R', 0, 0),
                ("logo.png", None, 'M', 0, 0),
                ("run.sh", None, 'M', 0, 0),
                ("with space.txt", None, 'M', 1, 1),
            ]
        );
        assert_eq!(
            text(&files[0]),
            "@@ -1,3 +1,4 @@\n one\n-two\n+deux\n+trois\n ",
            "from the first hunk on; the last context line keeps its space"
        );
        assert_eq!(files[4].patch, Patch::Renamed);
        assert_eq!(files[5].patch, Patch::Binary);
        assert_eq!(text(&files[6]), "", "only its mode changed");
        assert!(
            files
                .iter()
                .all(|f| f.viewed == Viewed::Unviewed && f.url.is_empty())
        );
    }

    #[test]
    fn nothing_odd_in_a_diff_panics() {
        assert!(split("").is_empty());
        assert!(split("@@ -1 +1 @@\n-a\n+b\n").is_empty(), "no file head");
        assert_eq!(split("diff --git a/x b/x\n").len(), 1);
        let one = split("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ garbage\n+a\n");
        assert_eq!(one[0].path, "x");
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A repo with `origin` one commit on: `main` has `a.txt` and `gone.txt`.
    #[cfg(unix)]
    fn repo(tmp: &Path) -> PathBuf {
        let origin = tmp.join("origin");
        std::fs::create_dir(&origin).unwrap();
        run(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(origin.join("gone.txt"), "bye\n").unwrap();
        run(&origin, &["add", "."]);
        run(&origin, &["commit", "-q", "-m", "one"]);
        let site = tmp.join("site");
        run(
            tmp,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                site.to_str().unwrap(),
            ],
        );
        run(&site, &["switch", "-q", "-c", "fix"]);
        site
    }

    #[cfg(unix)]
    fn names(r: &Read) -> Vec<(&str, char)> {
        r.data
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.change))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_whole_branch_or_only_the_uncommitted_with_new_files_too() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let git = crate::github::worktree::git_raw;
        // Committed on the branch: a.txt changed, gone.txt deleted.
        std::fs::write(site.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::remove_file(site.join("gone.txt")).unwrap();
        run(&site, &["commit", "-q", "-am", "work"]);
        // Not committed: a new file, a binary one, one too big to read.
        std::fs::write(site.join("new.md"), "hello\nworld\n").unwrap();
        std::fs::write(site.join("pic.bin"), b"\x00\x01\x02").unwrap();
        std::fs::write(
            site.join("big.log"),
            vec![b'x'; (NEW_FILE_LIMIT + 1) as usize],
        )
        .unwrap();
        let sub = site.join("src");
        std::fs::create_dir(&sub).unwrap();
        let all = read(&sub, DiffMode::Branch, None, &git).unwrap();
        assert_eq!(
            all.root,
            crate::place::resolved(&site),
            "from a folder inside: the repo"
        );
        assert_eq!(
            (all.data.head.as_str(), all.data.base.as_str()),
            ("fix", "origin/main")
        );
        assert!(all.data.dirty);
        assert_eq!(
            names(&all),
            [
                ("a.txt", 'M'),
                ("big.log", 'A'),
                ("gone.txt", 'D'),
                ("new.md", 'A'),
                ("pic.bin", 'A')
            ]
        );
        let new = &all.data.files[3];
        assert_eq!((new.additions, new.deletions), (2, 0));
        assert_eq!(
            new.patch,
            Patch::Text("@@ -0,0 +1,2 @@\n+hello\n+world".into())
        );
        assert_eq!(all.data.files[1].patch, Patch::TooLarge);
        assert_eq!(all.data.files[4].patch, Patch::Binary);
        assert_eq!(all.hashes["gone.txt"], "deleted");
        assert_eq!(all.hashes["a.txt"], content_hash(&all.root, "a.txt"));
        let now = read(&site, DiffMode::Uncommitted, None, &git).unwrap();
        assert_eq!(now.data.base, "HEAD");
        assert_eq!(
            names(&now),
            [("big.log", 'A'), ("new.md", 'A'), ("pic.bin", 'A')],
            "only what is not committed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_kept_base_is_used_and_no_merge_base_falls_back_to_the_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let git = crate::github::worktree::git_raw;
        std::fs::write(site.join("a.txt"), "changed\n").unwrap();
        let kept = read(&site, DiffMode::Branch, Some("main"), &git).unwrap();
        assert_eq!(kept.data.base, "main");
        // A branch with no history in common with main.
        run(&site, &["checkout", "-q", "--", "a.txt"]);
        run(&site, &["switch", "-q", "--orphan", "lone"]);
        run(&site, &["commit", "-q", "--allow-empty", "-m", "lone"]);
        std::fs::write(site.join("b.txt"), "b\n").unwrap();
        let lone = read(&site, DiffMode::Branch, None, &git).unwrap();
        assert_eq!(lone.data.base, "HEAD (no merge base with origin/main)");
        assert!(names(&lone).contains(&("b.txt", 'A')));
        assert_eq!(
            read(tmp.path(), DiffMode::Branch, None, &git).map(|r| r.root),
            Err("not a git repository".into())
        );
        assert_eq!(
            read(&tmp.path().join("nowhere"), DiffMode::Branch, None, &git).map(|r| r.root),
            Err("the folder is gone".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn names_with_spaces_and_turkish_letters_and_a_move_read_as_git_made_them() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let git = crate::github::worktree::git_raw;
        std::fs::write(site.join("çiçek ğ.txt"), "bir\n").unwrap();
        run(&site, &["add", "."]);
        run(&site, &["commit", "-q", "-m", "çiçek"]);
        run(&site, &["mv", "a.txt", "moved a.txt"]);
        std::fs::write(site.join("yeni dosya.md"), "x\n").unwrap();
        let r = read(&site, DiffMode::Branch, None, &git).unwrap();
        let sum: Vec<(&str, Option<&str>, char)> = r
            .data
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.previous.as_deref(), f.change))
            .collect();
        assert_eq!(
            sum,
            [
                ("moved a.txt", Some("a.txt"), 'R'),
                ("yeni dosya.md", None, 'A'),
                ("çiçek ğ.txt", None, 'A'),
            ]
        );
        assert_eq!(r.data.files[0].patch, Patch::Renamed);
        assert_eq!(
            r.hashes["çiçek ğ.txt"],
            content_hash(&r.root, "çiçek ğ.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn past_three_hundred_files_the_rest_are_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let git = crate::github::worktree::git_raw;
        for n in 0..=MAX_FILES {
            std::fs::write(site.join(format!("f{n:03}.txt")), "x\n").unwrap();
        }
        let r = read(&site, DiffMode::Uncommitted, None, &git).unwrap();
        assert_eq!((r.data.files.len(), r.data.more), (MAX_FILES, 1));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_new_files_kept_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        for n in 0..MAX_FILES + 100 {
            std::fs::write(site.join(format!("f{n:03}.txt")), "x\n").unwrap();
        }
        NEW_FILES_READ.with(|n| n.set(0));
        let r = read(
            &site,
            DiffMode::Uncommitted,
            None,
            &crate::github::worktree::git_raw,
        )
        .unwrap();
        assert_eq!((r.data.files.len(), r.data.more), (MAX_FILES, 100));
        assert_eq!(
            NEW_FILES_READ.with(|n| n.get()),
            MAX_FILES,
            "the rest are counted, not read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_fingerprint_moves_when_a_changed_file_changes_again() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let git = crate::github::worktree::git_raw;
        let clean = fingerprint(&site, &git).unwrap();
        assert_eq!(fingerprint(&site, &git), Some(clean), "nothing changed");
        std::fs::write(site.join("a.txt"), "one\n").unwrap();
        let once = fingerprint(&site, &git).unwrap();
        assert_ne!(once, clean);
        // git's status says the same `M a.txt` as before.
        std::fs::write(site.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let twice = fingerprint(&site, &git).unwrap();
        assert_ne!(twice, once);
        run(&site, &["commit", "-q", "-am", "c"]);
        assert_ne!(fingerprint(&site, &git).unwrap(), twice, "a commit");
        assert_eq!(fingerprint(&tmp.path().join("nowhere"), &git), None);
    }

    #[test]
    fn a_mark_holds_while_the_content_is_as_it_was() {
        let file = |path: &str| DiffFile {
            path: path.into(),
            previous: None,
            change: 'M',
            additions: 1,
            deletions: 0,
            viewed: Viewed::Unviewed,
            patch: Patch::Text(String::new()),
            url: String::new(),
        };
        let mut data = LocalDiffData {
            files: vec![file("a.rs"), file("b.rs"), file("c.rs")],
            ..LocalDiffData::default()
        };
        let hashes: HashMap<String, String> = [("a.rs", "1"), ("b.rs", "2"), ("c.rs", "3")]
            .map(|(f, h)| (f.to_string(), h.to_string()))
            .into();
        let marks: HashMap<String, String> = [("a.rs", "1"), ("b.rs", "old")]
            .map(|(f, h)| (f.to_string(), h.to_string()))
            .into();
        mark(&mut data, &hashes, &marks);
        let viewed: Vec<Viewed> = data.files.iter().map(|f| f.viewed).collect();
        assert_eq!(
            viewed,
            [Viewed::Viewed, Viewed::Dismissed, Viewed::Unviewed]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_content_hash_tells_one_content_from_another() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "one").unwrap();
        let first = content_hash(tmp.path(), "a");
        assert_eq!(first.len(), 64);
        assert_eq!(content_hash(tmp.path(), "a"), first);
        std::fs::write(tmp.path().join("a"), "two").unwrap();
        assert_ne!(content_hash(tmp.path(), "a"), first);
        assert_eq!(content_hash(tmp.path(), "missing"), "deleted");
    }

    #[cfg(unix)]
    #[test]
    fn only_marks_whose_files_are_as_they_were_count() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a"), "one").unwrap();
        std::fs::write(tmp.path().join("b"), "two").unwrap();
        let marks: HashMap<String, String> = [
            ("a".to_string(), content_hash(tmp.path(), "a")),
            ("b".to_string(), "an older b".to_string()),
            ("c".to_string(), content_hash(tmp.path(), "c")),
        ]
        .into();
        assert_eq!(
            count_reviewed(tmp.path(), &marks),
            2,
            "a, and c deleted as marked"
        );
    }
}
