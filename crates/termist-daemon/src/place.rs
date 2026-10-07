//! Where a session runs: what git says about its folder, matched with the project's
//! repos and their open pull requests.
use std::path::{Path, PathBuf};
use termist_core::Place;
use termist_core::github::{PrRef, RepoId};

/// What git says about a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFacts {
    /// The worktree's top folder.
    pub root: PathBuf,
    /// `None` when detached.
    pub branch: Option<String>,
    /// The commit when detached, short.
    pub commit: Option<String>,
    /// The repo's main folder (where its common `.git` lives), resolved, for matching
    /// a worktree with its repo.
    pub main: PathBuf,
}

/// A repo of the project with its open pull requests, as the inbox read them.
#[derive(Clone, Debug)]
pub struct RepoView {
    pub id: RepoId,
    /// Resolved like `GitFacts::main`.
    pub path: PathBuf,
    /// `owner/name`.
    pub slug: String,
    /// Each open pull request's number, head branch and head repo (`owner/name`).
    pub prs: Vec<(u32, String, String)>,
}

/// Asks git about `cwd`; `None` when it is not in a repo (or has no commit yet).
/// `git` runs a git command in a folder and returns its output when it succeeds.
pub fn read(cwd: &Path, git: &dyn Fn(&Path, &[&str]) -> Option<String>) -> Option<GitFacts> {
    let out = git(
        cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--abbrev-ref",
            "HEAD",
        ],
    )?;
    let mut lines = out.lines().map(str::trim);
    let (root, common, head) = (lines.next()?, lines.next()?, lines.next()?);
    let common = Path::new(common);
    // `<main>/.git`; a bare repo's common dir is the repo itself.
    let main = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent()?
    } else {
        common
    };
    let (branch, commit) = if head == "HEAD" {
        (None, git(cwd, &["rev-parse", "--short", "HEAD"]))
    } else {
        (Some(head.to_string()), None)
    };
    Some(GitFacts {
        root: PathBuf::from(root),
        branch,
        commit,
        main: resolved(main),
    })
}

/// The path with links and `..` resolved, for comparing; as it is when that fails.
pub fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The place of a session in `cwd`, from what git said and the project's repos.
pub fn place(cwd: &Path, facts: Option<&GitFacts>, repos: &[RepoView]) -> Place {
    let Some(facts) = facts else {
        return Place {
            root: cwd.to_path_buf(),
            branch: None,
            commit: None,
            repo: None,
            pr: None,
            gone: !cwd.exists(),
        };
    };
    let repo = repos.iter().find(|r| r.path == facts.main);
    let pr = match (repo, &facts.branch) {
        (Some(repo), Some(branch)) => {
            let heads = || repo.prs.iter().filter(|(_, head, _)| head == branch);
            // A fork may have a branch of the same name: the repo's own wins.
            heads()
                .find(|(_, _, from)| *from == repo.slug)
                .or_else(|| heads().next())
                .map(|(number, ..)| PrRef {
                    repo: repo.id,
                    number: *number,
                })
        }
        _ => None,
    };
    Place {
        root: facts.root.clone(),
        branch: facts.branch.clone(),
        commit: facts.commit.clone(),
        repo: repo.map(|r| r.id),
        pr,
        gone: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(answers: &[(&str, &str)]) -> impl Fn(&Path, &[&str]) -> Option<String> {
        let answers: Vec<(String, String)> = answers
            .iter()
            .map(|(a, o)| (a.to_string(), o.to_string()))
            .collect();
        move |_dir: &Path, args: &[&str]| {
            let asked = args.join(" ");
            answers
                .iter()
                .find(|(a, _)| asked.ends_with(a.as_str()))
                .map(|(_, o)| o.clone())
        }
    }

    #[test]
    fn git_tells_the_worktree_its_repo_and_its_branch() {
        let facts = read(
            Path::new("/w/site-worktrees/fix/login/src"),
            &git(&[(
                "--abbrev-ref HEAD",
                "/w/site-worktrees/fix/login\n/w/site/.git\nfix/login\n",
            )]),
        )
        .unwrap();
        assert_eq!(
            facts,
            GitFacts {
                root: "/w/site-worktrees/fix/login".into(),
                branch: Some("fix/login".into()),
                commit: None,
                main: "/w/site".into(),
            }
        );
    }

    #[test]
    fn a_detached_head_gives_its_commit_and_no_repo_gives_nothing() {
        let facts = read(
            Path::new("/w/site"),
            &git(&[
                ("--abbrev-ref HEAD", "/w/site\n/w/site/.git\nHEAD"),
                ("--short HEAD", "1a2b3c4"),
            ]),
        )
        .unwrap();
        assert_eq!(
            (facts.branch, facts.commit.as_deref()),
            (None, Some("1a2b3c4"))
        );
        assert_eq!(read(Path::new("/tmp"), &git(&[])), None);
    }

    fn site(prs: &[(u32, &str, &str)]) -> RepoView {
        RepoView {
            id: RepoId(7),
            path: "/w/site".into(),
            slug: "acme/site".into(),
            prs: prs
                .iter()
                .map(|(n, h, r)| (*n, h.to_string(), r.to_string()))
                .collect(),
        }
    }

    fn on(branch: Option<&str>) -> GitFacts {
        GitFacts {
            root: "/w/site-worktrees/x".into(),
            branch: branch.map(str::to_string),
            commit: None,
            main: "/w/site".into(),
        }
    }

    #[test]
    fn a_branch_finds_its_open_pull_request_the_repo_s_own_before_a_fork_s() {
        let repos = [site(&[
            (198, "main", "bob/site"),
            (212, "fix/login", "acme/site"),
            (230, "fix/login", "carol/site"),
        ])];
        let p = place(Path::new("/w/x"), Some(&on(Some("fix/login"))), &repos);
        assert_eq!(p.repo, Some(RepoId(7)));
        assert_eq!(p.pr.map(|pr| pr.number), Some(212));
        let p = place(Path::new("/w/x"), Some(&on(Some("main"))), &repos);
        assert_eq!(
            p.pr.map(|pr| pr.number),
            Some(198),
            "a fork's, when only it"
        );
        let p = place(Path::new("/w/x"), Some(&on(Some("docs"))), &repos);
        assert_eq!((p.repo, p.pr), (Some(RepoId(7)), None));
        let p = place(Path::new("/w/x"), Some(&on(None)), &repos);
        assert_eq!(p.pr, None, "detached");
    }

    #[test]
    fn a_folder_outside_the_project_s_repos_or_not_there_has_no_repo() {
        let mut other = on(Some("fix/login"));
        other.main = "/elsewhere/site".into();
        let repos = [site(&[(212, "fix/login", "acme/site")])];
        let p = place(Path::new("/w/x"), Some(&other), &repos);
        assert_eq!((p.repo, p.pr), (None, None));
        let gone = Path::new("/no/such/termist/folder");
        let p = place(gone, None, &repos);
        assert_eq!((p.root.as_path(), p.gone), (gone, true));
        let here = std::env::temp_dir();
        assert!(!place(&here, None, &repos).gone);
    }
}
