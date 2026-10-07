//! A repo's worktrees as git lists them, and what each one's branch changed since it
//! left its base: `3 files +60 −28`, and whether some of it is not committed.
use crate::github::worktree::Git;
use std::path::{Path, PathBuf};
use termist_core::Stat;

/// A worktree in `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    /// `None` when detached.
    pub branch: Option<String>,
    /// The repo's main checkout: the list's first entry.
    pub main: bool,
}

/// The worktrees that are there: one deleted without `git worktree prune` is listed as
/// `prunable` and left out, as is a bare repo.
pub fn parse_list(text: &str) -> Vec<Listed> {
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .enumerate()
        .filter_map(|(i, block)| {
            let path = block.lines().find_map(|l| l.strip_prefix("worktree "))?;
            let gone = block
                .lines()
                .any(|l| l.starts_with("prunable") || l.trim() == "bare");
            (!gone).then(|| Listed {
                path: PathBuf::from(path),
                branch: block
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("branch refs/heads/"))
                    .map(str::to_string),
                main: i == 0,
            })
        })
        .collect()
}

/// ` 3 files changed, 60 insertions(+), 28 deletions(-)` → (3, 60, 28); either count may
/// be missing, and an empty output is no change.
pub fn parse_shortstat(text: &str) -> (u32, u32, u32) {
    let mut out = (0, 0, 0);
    for part in text.split(',') {
        let mut words = part.split_whitespace();
        let Some(n) = words.next().and_then(|n| n.parse().ok()) else {
            continue;
        };
        match words.next() {
            Some(w) if w.starts_with("file") => out.0 = n,
            Some(w) if w.starts_with("insertion") => out.1 = n,
            Some(w) if w.starts_with("deletion") => out.2 = n,
            _ => {}
        }
    }
    out
}

/// The repo's default branch: what `origin/HEAD` points at, else `main` or `master`
/// if there, else the branch the repo is on.
pub fn default_branch(repo: &Path, git: Git) -> String {
    if let Ok(head) = git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(name) = head.trim().strip_prefix("origin/")
        && !name.is_empty()
    {
        return name.to_string();
    }
    for name in ["main", "master"] {
        if git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ],
        )
        .is_ok()
        {
            return name.to_string();
        }
    }
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD")
        .unwrap_or_else(|| "main".into())
}

/// What the worktree at `path` changed since it left `base` (a ref), the uncommitted
/// too; `None` when there is no merge base.
pub fn stat(path: &Path, base: &str, git: Git) -> Option<Stat> {
    let since = git(path, &["merge-base", "HEAD", base]).ok()?;
    let since = since.trim();
    if since.is_empty() {
        return None;
    }
    let short = git(path, &["diff", "--shortstat", since]).ok()?;
    let (files, added, removed) = parse_shortstat(&short);
    let dirty = git(path, &["status", "--porcelain"]).is_ok_and(|s| !s.trim().is_empty());
    Some(Stat {
        files,
        added,
        removed,
        dirty,
    })
}

/// A repo's worktrees but its main checkout and the project's own folder, each with
/// what its branch changed; `base` names a worktree's base branch (else the default's
/// `origin/` one is used, or the local one when there is no `origin`).
pub fn scan(
    repo: &Path,
    own: &Path,
    base: &dyn Fn(&Path) -> Option<String>,
    git: Git,
) -> Option<Vec<(Listed, Option<Stat>)>> {
    let list = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let default = default_branch(repo, git);
    let origin = format!("origin/{default}");
    let fallback = match git(repo, &["rev-parse", "--verify", "--quiet", &origin]) {
        Ok(_) => origin,
        Err(_) => default,
    };
    let own = crate::place::resolved(own);
    Some(
        parse_list(&list)
            .into_iter()
            .filter(|w| !w.main && crate::place::resolved(&w.path) != own)
            .map(|w| {
                let against = base(&w.path).unwrap_or_else(|| fallback.clone());
                let stat = stat(&w.path, &against, git);
                (w, stat)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_names_each_worktree_its_branch_and_the_main_one() {
        let list = "worktree /w/site\nHEAD 1a2b\nbranch refs/heads/main\n\n\
                    worktree /w/site-worktrees/fix/login\nHEAD 3c4d\nbranch refs/heads/fix/login\n\n\
                    worktree /w/site-worktrees/old\nHEAD 5e6f\ndetached\n\n\
                    worktree /w/gone\nHEAD 7a8b\nbranch refs/heads/gone\nprunable gitdir file points to non-existent location\n";
        assert_eq!(
            parse_list(list),
            [
                Listed {
                    path: "/w/site".into(),
                    branch: Some("main".into()),
                    main: true
                },
                Listed {
                    path: "/w/site-worktrees/fix/login".into(),
                    branch: Some("fix/login".into()),
                    main: false
                },
                Listed {
                    path: "/w/site-worktrees/old".into(),
                    branch: None,
                    main: false
                },
            ]
        );
    }

    #[test]
    fn shortstat_reads_files_insertions_and_deletions_whichever_are_there() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 60 insertions(+), 28 deletions(-)\n"),
            (3, 60, 28)
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+)"),
            (1, 1, 0)
        );
        assert_eq!(
            parse_shortstat(" 2 files changed, 5 deletions(-)"),
            (2, 0, 5)
        );
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[cfg(unix)]
    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_worktree_s_branch_counts_its_commits_and_its_uncommitted_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir(&site).unwrap();
        run(&site, &["init", "-q", "-b", "main"]);
        std::fs::write(site.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(&site, &["add", "."]);
        run(&site, &["commit", "-q", "-m", "init"]);
        let fix = tmp.path().join("site-worktrees").join("fix");
        run(
            &site,
            &["worktree", "add", "-q", "-b", "fix", fix.to_str().unwrap()],
        );
        // A commit on the branch, then a change not committed.
        std::fs::write(fix.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        run(&fix, &["commit", "-q", "-am", "work"]);
        std::fs::write(fix.join("b.txt"), "new\n").unwrap();
        run(&fix, &["add", "b.txt"]);
        let git = crate::github::worktree::git;
        assert_eq!(
            default_branch(&site, &git),
            "main",
            "no origin: the local main"
        );
        let found = scan(&site, &site, &|_| None, &git).unwrap();
        assert_eq!(found.len(), 1, "the main checkout is not one of them");
        let (listed, stat) = &found[0];
        assert_eq!(listed.branch.as_deref(), Some("fix"));
        assert_eq!(
            *stat,
            Some(Stat {
                files: 2,
                added: 3,
                removed: 1,
                dirty: true
            })
        );
        // The project's own folder is the project's band, not a worktree of it.
        assert!(scan(&site, &fix, &|_| None, &git).unwrap().is_empty());
    }
}
