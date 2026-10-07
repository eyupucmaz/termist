//! A worktree on a pull request's branch: the one already there, else a new folder
//! beside the repo (`<repo>/../<repo>-worktrees/<branch>`) that `gh pr checkout` fills.
use super::gh::{Gh, classify};
use super::said;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs git in a folder: its output, or the first line of what it said when it failed.
pub type Git<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, String>;

/// The worktree for pull request `number` of the repo at `repo`, whose head is `branch`;
/// `true` when it was made now.
pub fn open(
    repo: &Path,
    branch: &str,
    number: u32,
    git: Git,
    gh: &dyn Gh,
    token: &str,
) -> Result<(PathBuf, bool), String> {
    let list = git(repo, &["worktree", "list", "--porcelain"])?;
    if let Some(path) = with_branch(&list, branch) {
        return Ok((path, false));
    }
    let dest = free(&folder(repo, branch));
    let dest_text = dest.to_string_lossy().into_owned();
    git(repo, &["worktree", "add", "--detach", &dest_text])?;
    let number = number.to_string();
    let checkout = gh
        .run_in(&dest, &["pr", "checkout", &number], Some(token))
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

/// The worktree that has `branch` checked out, from `git worktree list --porcelain`.
fn with_branch(list: &str, branch: &str) -> Option<PathBuf> {
    let want = format!("branch refs/heads/{branch}");
    list.split("\n\n").find_map(|block| {
        let path = block.lines().find_map(|l| l.strip_prefix("worktree "))?;
        block
            .lines()
            .any(|l| l.trim() == want)
            .then(|| PathBuf::from(path))
    })
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
fn free(dir: &Path) -> PathBuf {
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

    fn checkout(ok: bool) -> Checkout {
        Checkout {
            ok,
            ran: Mutex::new(vec![]),
        }
    }

    const LIST: &str = "worktree /w/site\nHEAD 1a2b\nbranch refs/heads/main\n\n\
                        worktree /w/site-worktrees/fix/login\nHEAD 3c4d\nbranch refs/heads/fix/login\n\n\
                        worktree /w/site-worktrees/old\nHEAD 5e6f\ndetached\n";

    #[test]
    fn a_branch_already_checked_out_is_used_where_it_is() {
        let gh = checkout(true);
        let asked = RefCell::new(vec![]);
        let git = |_: &Path, args: &[&str]| {
            asked.borrow_mut().push(args.join(" "));
            Ok(LIST.to_string())
        };
        let got = open(Path::new("/w/site"), "fix/login", 212, &git, &gh, "t");
        assert_eq!(got, Ok(("/w/site-worktrees/fix/login".into(), false)));
        assert_eq!(
            open(Path::new("/w/site"), "main", 3, &git, &gh, "t")
                .unwrap()
                .0,
            PathBuf::from("/w/site")
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
        let (path, made) = open(&repo, "fix/login", 212, &git, &gh, "tok").unwrap();
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
            (want, "pr checkout 212".into(), Some("tok".into())),
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
        let got = open(&repo, "fix/login", 212, &git, &gh, "tok");
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
            open(&repo, "x", 1, &refused, &checkout(true), "t"),
            Err("fatal: invalid reference".into())
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
