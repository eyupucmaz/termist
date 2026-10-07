//! A worktree on a pull request's branch: the one already there, else a new folder
//! beside the repo (`<repo>/../<repo>-worktrees/<branch>`) that `gh pr checkout` fills.
use super::gh::{Gh, classify};
use super::said;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs git in a folder: its output, or the first line of what it said when it failed.
pub type Git<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, String>;

/// What a pull request's worktree is for: its head branch, its number, the repo's
/// `owner/name`, and whether the head lives in another repo (a fork).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub branch: String,
    pub number: u32,
    pub slug: String,
    pub fork: bool,
}

/// The worktree for pull request `head` of the repo at `repo`; `true` when it was made
/// now.
pub fn open(
    repo: &Path,
    head: &Head,
    git: Git,
    gh: &dyn Gh,
    token: &str,
) -> Result<(PathBuf, bool), String> {
    let (branch, number) = (head.branch.as_str(), head.number);
    let list = git(repo, &["worktree", "list", "--porcelain"])?;
    let found = match head.fork {
        // A fork's `main` is not the clone's `main`: only a folder made for it counts.
        true => made_for(&list, &folder(repo, branch)),
        false => with_branch(&list, branch),
    };
    if let Some(path) = found {
        return Ok((path, false));
    }
    let dest = free(&folder(repo, branch));
    let dest_text = dest.to_string_lossy().into_owned();
    git(repo, &["worktree", "add", "--detach", &dest_text])?;
    let number = number.to_string();
    let checkout = gh
        // The repo named: a clone with two remotes leaves gh asking which one.
        .run_in(
            &dest,
            &["pr", "checkout", &number, "-R", &head.slug],
            Some(token),
        )
        .map_err(|state| said(&state))
        .and_then(|out| match out.success {
            true => Ok(()),
            false => Err(said(&classify(&out))),
        });
    if let Err(why) = checkout {
        // Only the folder made just now, with nothing in it but the checkout.
        let _ = git(repo, &["worktree", "remove", "--force", &dest_text]);
        return Err(why);
    }
    Ok((dest, true))
}

/// The worktrees in `git worktree list --porcelain` that are there: one whose folder
/// was deleted without `git worktree prune` is listed as `prunable`.
fn present(list: &str) -> impl Iterator<Item = (PathBuf, &str)> {
    list.split("\n\n").filter_map(|block| {
        let path = PathBuf::from(block.lines().find_map(|l| l.strip_prefix("worktree "))?);
        let gone = block.lines().any(|l| l.starts_with("prunable")) || !path.exists();
        (!gone).then_some((path, block))
    })
}

/// The worktree that has `branch` checked out.
fn with_branch(list: &str, branch: &str) -> Option<PathBuf> {
    let want = format!("branch refs/heads/{branch}");
    present(list)
        .find(|(_, block)| block.lines().any(|l| l.trim() == want))
        .map(|(path, _)| path)
}

/// The worktree termist made at `dir` (or `dir-2`, `dir-3`… when the name was taken).
fn made_for(list: &str, dir: &Path) -> Option<PathBuf> {
    present(list)
        .find(|(path, _)| is_made_for(path, dir))
        .map(|(path, _)| path)
}

/// `path` is `dir` or a numbered `dir-n` beside it.
pub fn is_made_for(path: &Path, dir: &Path) -> bool {
    let (Some(name), Some(want)) = (path.file_name(), dir.file_name()) else {
        return false;
    };
    let (name, want) = (name.to_string_lossy(), want.to_string_lossy());
    path.parent() == dir.parent()
        && (name == want
            || name
                .strip_prefix(&*want)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|n| n.parse::<u32>().is_ok()))
}

/// `<repo>/../<repo>-worktrees/<branch>`, each part of the branch a folder, with the
/// characters a file name cannot hold on some system made `-`.
pub fn folder(repo: &Path, branch: &str) -> PathBuf {
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let mut dir = repo
        .parent()
        .unwrap_or(repo)
        .join(format!("{name}-worktrees"));
    for part in branch.split('/') {
        let part: String = part
            .chars()
            .map(|c| match c {
                ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\\' => '-',
                c if c.is_control() => '-',
                c => c,
            })
            .collect();
        dir.push(match part.as_str() {
            "" | "." | ".." => "-",
            _ => part.as_str(),
        });
    }
    dir
}

/// `dir`, or `dir-2`, `dir-3`… when something is there already.
pub fn free(dir: &Path) -> PathBuf {
    if !dir.exists() {
        return dir.to_path_buf();
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (2..)
        .map(|n| dir.with_file_name(format!("{name}-{n}")))
        .find(|d| !d.exists())
        .expect("a free name")
}

/// Git as a program; what it said on failure, first line.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut all = vec!["-C".to_string(), dir.to_string_lossy().into_owned()];
    all.extend(args.iter().map(|a| a.to_string()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let out =
        termist_platform::process::run(Path::new("git"), &all, &[], None, Duration::from_secs(60))
            .map_err(|e| format!("could not run git: {e:?}"))?;
    if out.success {
        return Ok(out.stdout.trim().to_string());
    }
    Err(out
        .stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("git failed")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::gh::GhOutput;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use termist_core::github::GhState;

    /// gh that records where it ran and answers as told.
    struct Checkout {
        ok: bool,
        ran: Mutex<Vec<(PathBuf, String, Option<String>)>>,
    }

    impl Gh for Checkout {
        fn run(&self, _: &[&str], _: Option<&str>, _: Option<&str>) -> Result<GhOutput, GhState> {
            unreachable!("only run_in")
        }

        fn run_in(
            &self,
            dir: &Path,
            args: &[&str],
            token: Option<&str>,
        ) -> Result<GhOutput, GhState> {
            self.ran.lock().unwrap().push((
                dir.to_path_buf(),
                args.join(" "),
                token.map(str::to_string),
            ));
            Ok(GhOutput {
                success: self.ok,
                stdout: String::new(),
                stderr: if self.ok {
                    String::new()
                } else {
                    "could not find pull request 212\n".into()
                },
            })
        }
    }

    fn at(branch: &str, fork: bool) -> Head {
        Head {
            branch: branch.into(),
            number: 212,
            slug: "acme/site".into(),
            fork,
        }
    }

    fn checkout(ok: bool) -> Checkout {
        Checkout {
            ok,
            ran: Mutex::new(vec![]),
        }
    }

    #[test]
    fn a_branch_already_checked_out_is_used_where_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let (site, fix) = (
            tmp.path().join("site"),
            tmp.path().join("site-worktrees/fix/login"),
        );
        std::fs::create_dir_all(&site).unwrap();
        std::fs::create_dir_all(&fix).unwrap();
        let list = format!(
            "worktree {}\nHEAD 1a2b\nbranch refs/heads/main\n\n\
             worktree {}\nHEAD 3c4d\nbranch refs/heads/fix/login\n\n\
             worktree {}\nHEAD 5e6f\ndetached\n",
            site.display(),
            fix.display(),
            tmp.path().join("site-worktrees/old").display()
        );
        let gh = checkout(true);
        let asked = RefCell::new(vec![]);
        let git = |_: &Path, args: &[&str]| {
            asked.borrow_mut().push(args.join(" "));
            Ok(list.clone())
        };
        let got = open(&site, &at("fix/login", false), &git, &gh, "t");
        assert_eq!(got, Ok((fix, false)));
        assert_eq!(
            open(&site, &at("main", false), &git, &gh, "t").unwrap().0,
            site
        );
        assert_eq!(asked.borrow().len(), 2, "nothing added");
        assert!(gh.ran.lock().unwrap().is_empty());
    }

    #[test]
    fn a_new_worktree_is_added_detached_beside_the_repo_and_checked_out_by_gh() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("site");
        let gh = checkout(true);
        let asked = RefCell::new(vec![]);
        let git = |dir: &Path, args: &[&str]| {
            asked.borrow_mut().push((dir.to_path_buf(), args.join(" ")));
            Ok(String::new())
        };
        let (path, made) = open(&repo, &at("fix/login", false), &git, &gh, "tok").unwrap();
        let want = tmp.path().join("site-worktrees").join("fix").join("login");
        assert_eq!((path.clone(), made), (want.clone(), true));
        assert_eq!(
            asked.borrow()[1],
            (
                repo.clone(),
                format!("worktree add --detach {}", want.display())
            )
        );
        assert_eq!(
            gh.ran.lock().unwrap()[0],
            (
                want,
                "pr checkout 212 -R acme/site".into(),
                Some("tok".into())
            ),
            "in the new folder, as the repo's account"
        );
    }

    #[test]
    fn a_failed_checkout_removes_the_folder_it_made_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("site");
        let gh = checkout(false);
        let asked = RefCell::new(vec![]);
        let git = |_: &Path, args: &[&str]| {
            asked.borrow_mut().push(args.join(" "));
            Ok(String::new())
        };
        let got = open(&repo, &at("fix/login", false), &git, &gh, "tok");
        assert_eq!(got, Err("could not find pull request 212".into()));
        let dest = tmp.path().join("site-worktrees").join("fix").join("login");
        assert_eq!(
            asked.borrow().last().unwrap(),
            &format!("worktree remove --force {}", dest.display())
        );
        let refused = |_: &Path, args: &[&str]| match args[0] {
            "worktree" if args[1] == "add" => Err("fatal: invalid reference".to_string()),
            _ => Ok(String::new()),
        };
        assert_eq!(
            open(&repo, &at("x", false), &refused, &checkout(true), "t"),
            Err("fatal: invalid reference".into())
        );
    }

    #[test]
    fn a_worktree_whose_folder_was_deleted_is_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("site");
        let gone = tmp.path().join("site-worktrees").join("fix");
        let list = format!(
            "worktree {}\nHEAD 1a2b\nbranch refs/heads/main\n\n\
             worktree {}\nHEAD 3c4d\nbranch refs/heads/fix\nprunable gitdir file points to non-existent location\n",
            repo.display(),
            gone.display()
        );
        let git = |_: &Path, args: &[&str]| match args[1] {
            "list" => Ok(list.clone()),
            _ => Ok(String::new()),
        };
        let gh = checkout(true);
        let (path, made) = open(&repo, &at("fix", false), &git, &gh, "t").unwrap();
        assert!(
            made,
            "made again rather than handing out a folder that is not there"
        );
        assert_eq!(path, gone);
    }

    #[test]
    fn a_fork_s_branch_is_looked_for_only_in_the_folders_termist_made_for_it() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("site");
        let ours = tmp.path().join("site-worktrees").join("main");
        let list = |with_ours: bool| {
            let mut l = format!(
                "worktree {}\nHEAD 1a2b\nbranch refs/heads/main\n",
                repo.display()
            );
            if with_ours {
                std::fs::create_dir_all(&ours).unwrap();
                l += &format!(
                    "\nworktree {}\nHEAD 9f9f\nbranch refs/heads/bob-main\n",
                    ours.display()
                );
            }
            l
        };
        let gh = checkout(true);
        let fresh = list(false);
        let git = |_: &Path, args: &[&str]| match args[1] {
            "list" => Ok(fresh.clone()),
            _ => Ok(String::new()),
        };
        let (path, made) = open(&repo, &at("main", true), &git, &gh, "t").unwrap();
        assert_eq!(
            (path, made),
            (ours.clone(), true),
            "not the clone's own main"
        );
        let again = list(true);
        let git = |_: &Path, args: &[&str]| match args[1] {
            "list" => Ok(again.clone()),
            _ => Ok(String::new()),
        };
        assert_eq!(
            open(&repo, &at("main", true), &git, &gh, "t").unwrap(),
            (ours, false),
            "the one made for it before"
        );
    }

    #[test]
    fn the_folder_holds_the_branch_safely_and_a_taken_name_gets_a_number() {
        let repo = Path::new("/w/site");
        assert_eq!(
            folder(repo, "alice/fix:login?"),
            Path::new("/w/site-worktrees/alice/fix-login-")
        );
        assert_eq!(folder(repo, "../up"), Path::new("/w/site-worktrees/-/up"));
        let tmp = tempfile::tempdir().unwrap();
        let taken = tmp.path().join("fix");
        std::fs::create_dir(&taken).unwrap();
        std::fs::create_dir(tmp.path().join("fix-2")).unwrap();
        assert_eq!(free(&taken), tmp.path().join("fix-3"));
        assert_eq!(free(&tmp.path().join("new")), tmp.path().join("new"));
    }
}
