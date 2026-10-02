//! Finding a project's GitHub repos: the repo the folder is, else the repos one level
//! below it, else the repo the folder is inside.
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRepo {
    pub path: PathBuf,
    /// The folder's name, as the inbox shows it.
    pub name: String,
    pub owner: String,
    pub repo: String,
}

/// `owner` and `name` of a github.com remote URL, in any of git's forms.
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    // Credentials in an https remote are not part of the address.
    let url = match url.split_once("://") {
        Some((scheme, rest)) => {
            let rest = match rest.split_once('@') {
                Some((user, host)) if !user.contains('/') => host,
                _ => rest,
            };
            format!("{scheme}://{rest}")
        }
        None => url.to_string(),
    };
    let path = [
        "https://github.com/",
        "http://github.com/",
        "ssh://github.com/",
        "git://github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let fine = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (fine(owner) && fine(name)).then(|| (owner.to_string(), name.to_string()))
}

/// The project's GitHub repos. `git` runs a git command in a folder and returns its
/// output when it succeeds (`git` below, a stand-in in tests).
pub fn discover(project: &Path, git: &dyn Fn(&Path, &[&str]) -> Option<String>) -> Vec<LocalRepo> {
    if project.join(".git").exists() {
        return repo_at(project, git).into_iter().collect();
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(project)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        // `file_type` does not follow links: a linked folder is not a child repo.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    children.sort();
    let found: Vec<LocalRepo> = children.iter().filter_map(|d| repo_at(d, git)).collect();
    if !found.is_empty() {
        return found;
    }
    git(project, &["rev-parse", "--show-toplevel"])
        .and_then(|top| repo_at(Path::new(top.trim()), git))
        .into_iter()
        .collect()
}

/// The GitHub repo at `dir`: its `origin`, else its `upstream`.
fn repo_at(dir: &Path, git: &dyn Fn(&Path, &[&str]) -> Option<String>) -> Option<LocalRepo> {
    let (owner, repo) = ["origin", "upstream"]
        .iter()
        .filter_map(|remote| git(dir, &["remote", "get-url", remote]))
        .find_map(|url| parse_remote(&url))?;
    Some(LocalRepo {
        path: dir.to_path_buf(),
        name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.clone()),
        owner,
        repo,
    })
}

pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut all = vec!["-C", dir.to_str()?];
    all.extend_from_slice(args);
    let out =
        termist_platform::process::run(Path::new("git"), &all, &[], None, Duration::from_secs(5))
            .ok()?;
    let text = out.stdout.trim();
    (out.success && !text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn remotes_in_every_github_form() {
        let want = Some(("acme".to_string(), "site".to_string()));
        for url in [
            "https://github.com/acme/site",
            "https://github.com/acme/site.git",
            "https://github.com/acme/site/",
            "http://github.com/acme/site.git",
            "https://alice:tok@github.com/acme/site.git",
            "git@github.com:acme/site.git",
            "ssh://git@github.com/acme/site.git",
            "git://github.com/acme/site.git",
            "  git@github.com:acme/site\n",
        ] {
            assert_eq!(parse_remote(url), want, "{url}");
        }
        for url in [
            "",
            "https://gitlab.com/acme/site.git",
            "https://github.com.evil.com/acme/site",
            "https://github.com/acme",
            "https://github.com/acme/site/tree/main",
            "/srv/git/site.git",
        ] {
            assert_eq!(parse_remote(url), None, "{url}");
        }
    }

    /// A stand-in git: remotes by folder, and the repo a folder is inside.
    struct FakeGit {
        origin: HashMap<PathBuf, String>,
        upstream: HashMap<PathBuf, String>,
        top: HashMap<PathBuf, PathBuf>,
    }

    impl FakeGit {
        fn new() -> FakeGit {
            FakeGit {
                origin: HashMap::new(),
                upstream: HashMap::new(),
                top: HashMap::new(),
            }
        }

        fn answer(&self, dir: &Path, args: &[&str]) -> Option<String> {
            match args {
                ["remote", "get-url", "origin"] => self.origin.get(dir).cloned(),
                ["remote", "get-url", "upstream"] => self.upstream.get(dir).cloned(),
                ["rev-parse", "--show-toplevel"] => {
                    self.top.get(dir).map(|p| p.display().to_string())
                }
                _ => None,
            }
        }
    }

    fn repo_dir(parent: &Path, name: &str, dot_git_file: bool) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if dot_git_file {
            std::fs::write(dir.join(".git"), "gitdir: /elsewhere").unwrap();
        } else {
            std::fs::create_dir(dir.join(".git")).unwrap();
        }
        dir
    }

    #[test]
    fn a_repo_folder_is_its_own_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo_dir(tmp.path(), "site", false);
        let mut git = FakeGit::new();
        git.origin
            .insert(site.clone(), "git@github.com:acme/site.git".into());
        let found = discover(&site, &|d, a| git.answer(d, a));
        assert_eq!(
            found,
            [LocalRepo {
                path: site,
                name: "site".into(),
                owner: "acme".into(),
                repo: "site".into()
            }]
        );
    }

    #[test]
    fn a_folder_of_repos_lists_each_github_repo_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let site = repo_dir(root, "site", false);
        let admin = repo_dir(root, "admin", true); // a worktree: .git is a file
        let lab = repo_dir(root, "lab", false);
        let hidden = repo_dir(root, ".tools", false);
        std::fs::create_dir(root.join("notes")).unwrap();
        std::fs::write(root.join("README.md"), "x").unwrap();
        let mut git = FakeGit::new();
        git.origin
            .insert(site.clone(), "https://github.com/acme/site.git".into());
        git.upstream
            .insert(admin.clone(), "git@github.com:acme/admin.git".into());
        git.origin
            .insert(lab, "https://gitlab.com/acme/lab.git".into());
        git.origin
            .insert(hidden, "https://github.com/acme/tools.git".into());
        let found = discover(root, &|d, a| git.answer(d, a));
        let names: Vec<_> = found.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["admin", "site"]);
        assert_eq!(found[0].repo, "admin");
    }

    #[test]
    fn a_subfolder_of_a_repo_uses_that_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let mono = repo_dir(tmp.path(), "mono", false);
        let web = mono.join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        let mut git = FakeGit::new();
        git.origin
            .insert(mono.clone(), "https://github.com/acme/mono".into());
        git.top.insert(web.clone(), mono.clone());
        let found = discover(&web, &|d, a| git.answer(d, a));
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].path.as_path(), found[0].name.as_str()),
            (mono.as_path(), "mono")
        );
    }

    #[test]
    fn child_repos_win_over_a_repo_around_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let code = home.join("code");
        let site = repo_dir(&code, "site", false);
        let mut git = FakeGit::new();
        git.origin
            .insert(home.clone(), "https://github.com/alice/dotfiles".into());
        git.top.insert(code.clone(), home);
        git.origin
            .insert(site, "https://github.com/acme/site".into());
        let found = discover(&code, &|d, a| git.answer(d, a));
        let names: Vec<_> = found.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(names, ["site"]);
    }

    #[test]
    fn a_folder_without_github_repos_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let git = FakeGit::new();
        assert!(discover(tmp.path(), &|d, a| git.answer(d, a)).is_empty());
        assert!(discover(&tmp.path().join("missing"), &|d, a| git.answer(d, a)).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_real_git_reads_a_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap()
        };
        if !run(&["init", "-q"]).status.success() {
            return; // no git on this machine
        }
        run(&[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/site.git",
        ]);
        let found = discover(dir, &git);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].owner.as_str(), found[0].repo.as_str()),
            ("acme", "site")
        );
    }
}
