//! `f` and `F`: a repo's files, and the lines `git grep` finds in them.
use std::path::{Path, PathBuf};
use std::time::Duration;
use termist_core::GrepMatch;
use termist_platform::process::{RunError, run};

/// Files past this many are counted, not sent.
pub const MAX_FILES: usize = 200_000;
/// Paths past this many bytes, all together, are counted, not sent: a message has
/// to stay well inside the 16 MiB a frame may be.
pub const MAX_BYTES: usize = 8 << 20;
/// Lines past this many are not sent.
pub const MAX_MATCHES: usize = 500;
/// A line found is cut to this many characters.
pub const LINE_CHARS: usize = 200;
/// `git grep` past this long is stopped.
pub const GREP_FOR: Duration = Duration::from_secs(10);

/// Runs git in `dir` for at most `limit`; its output, or what it said went wrong. A
/// run that fails saying nothing (`git grep` finding nothing) is an empty output.
fn git(dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut all = vec!["-C".to_string(), dir.to_string_lossy().into_owned()];
    all.extend(args.iter().map(|a| a.to_string()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let out = run(Path::new("git"), &all, &[], None, limit).map_err(|e| match e {
        RunError::TimedOut => "took too long".to_string(),
        other => format!("could not run git: {other}"),
    })?;
    if out.success {
        return Ok(out.stdout);
    }
    match out.stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
        None => Ok(String::new()),
        Some(why) => Err(why.to_string()),
    }
}

/// The first `max` lines git prints in `dir`, read as they come: git is stopped once
/// there are more (a short query in a big repo prints a great deal), or after
/// `GREP_FOR`. Whether there were more.
fn first_lines(dir: &Path, args: &[&str], max: usize) -> Result<(String, bool), String> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let (stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let late = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Past the time allowed, git is stopped; the reading below then ends.
    {
        let (child, done, late) = (child.clone(), done.clone(), late.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + GREP_FOR;
            while std::time::Instant::now() < deadline {
                if done.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            late.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
            }
        });
    }
    let said = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(err) = stderr.as_mut() {
            let _ = err.read_to_string(&mut text);
        }
        text
    });
    let mut out = String::new();
    let mut more = false;
    if let Some(stdout) = stdout {
        let mut lines = BufReader::new(stdout).lines();
        let mut n = 0;
        while let Some(Ok(line)) = lines.next() {
            if n == max {
                more = true;
                break;
            }
            out.push_str(&line);
            out.push('\n');
            n += 1;
        }
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let status = child.lock().ok().and_then(|mut c| {
        if more {
            let _ = c.kill();
        }
        c.wait().ok()
    });
    if late.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("grep took too long".into());
    }
    let said = said.join().unwrap_or_default();
    // Nothing found is not a failure; something said on a failed run is.
    if !more
        && status.is_some_and(|s| !s.success())
        && let Some(why) = said.lines().map(str::trim).find(|l| !l.is_empty())
    {
        return Err(why.to_string());
    }
    Ok((out, more))
}

/// The root of the repo `folder` is in.
fn root(folder: &Path) -> Result<PathBuf, String> {
    let top = git(folder, &["rev-parse", "--show-toplevel"], GREP_FOR)
        .map_err(|_| "not a git repository".to_string())?;
    let top = top.trim();
    if top.is_empty() {
        return Err("not a git repository".into());
    }
    Ok(crate::place::resolved(Path::new(top)))
}

/// The repo's files (tracked, and new ones not ignored), relative to its root; with
/// how many past `MAX_FILES` were left out.
pub fn files(folder: &Path) -> Result<(PathBuf, Vec<String>, u32), String> {
    let root = root(folder)?;
    let out = git(
        &root,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
        ],
        Duration::from_secs(60),
    )?;
    let names: Vec<String> = out
        .split('\0')
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();
    let (names, more) = keep(names);
    Ok((root, names, more))
}

/// The files that are sent, at most `MAX_FILES` and `MAX_BYTES`; with how many more.
pub fn keep(mut names: Vec<String>) -> (Vec<String>, u32) {
    let total = names.len();
    let mut bytes = 0;
    let fit = names
        .iter()
        .take(MAX_FILES)
        .take_while(|n| {
            bytes += n.len();
            bytes <= MAX_BYTES
        })
        .count();
    names.truncate(fit);
    (names, (total - fit) as u32)
}

#[cfg(test)]
thread_local! {
    /// How many lines `grep` read from git, for the tests.
    static GREP_LINES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `git grep --null -n` output: `path\0line\0text` a line.
pub fn parse_grep(text: &str) -> Vec<GrepMatch> {
    text.lines()
        .filter_map(|l| {
            #[cfg(test)]
            GREP_LINES_READ.with(|n| n.set(n.get() + 1));
            let mut parts = l.splitn(3, '\0');
            let path = parts.next()?;
            let line = parts.next()?.parse().ok()?;
            let text = parts.next()?;
            Some(GrepMatch {
                path: path.to_string(),
                line,
                text: text.chars().take(LINE_CHARS).collect(),
            })
        })
        .collect()
}

/// The lines with `query` in the repo's files (text, not a pattern; any case unless it
/// has a capital), new files too; whether there were more than `MAX_MATCHES`.
pub fn grep(folder: &Path, query: &str) -> Result<(PathBuf, Vec<GrepMatch>, bool), String> {
    let root = root(folder)?;
    let mut args = vec![
        "-c",
        "core.quotePath=false",
        "grep",
        "-n",
        "-I",
        "--null",
        "--no-color",
        "--untracked",
        "-F",
    ];
    if !query.chars().any(char::is_uppercase) {
        args.push("-i");
    }
    // `-e` keeps a query that starts with `-` a query.
    args.extend(["-e", query]);
    let (out, more) = first_lines(&root, &args, MAX_MATCHES)?;
    let mut found = parse_grep(&out);
    found.truncate(MAX_MATCHES);
    Ok((root, found, more))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_list_stays_well_inside_one_message() {
        // 1,000 paths of 10,000 bytes: 10 MB, more than a list may carry.
        let long: Vec<String> = (0..1000)
            .map(|n| format!("{n:05}{}", "x".repeat(9995)))
            .collect();
        let (kept, more) = keep(long);
        let bytes: usize = kept.iter().map(String::len).sum();
        assert!(bytes <= MAX_BYTES, "{bytes}");
        assert_eq!(kept.len() as u32 + more, 1000, "the rest are counted");
        let many: Vec<String> = (0..MAX_FILES + 3).map(|n| n.to_string()).collect();
        let (kept, more) = keep(many);
        assert_eq!((kept.len(), more), (MAX_FILES, 3));
    }

    #[test]
    fn a_grep_line_is_its_path_number_and_text_cut_to_fit() {
        let long = "x".repeat(LINE_CHARS + 50);
        let out = format!(
            "src/a.rs\u{0}42\u{0}  let redirect = q;\ndir with space/b.md\u{0}7\u{0}{long}\nodd line\n"
        );
        let found = parse_grep(&out);
        assert_eq!(found.len(), 2, "an odd line is skipped");
        assert_eq!(
            found[0],
            GrepMatch {
                path: "src/a.rs".into(),
                line: 42,
                text: "  let redirect = q;".into()
            }
        );
        assert_eq!(found[1].path, "dir with space/b.md");
        assert_eq!(found[1].text.chars().count(), LINE_CHARS);
    }

    #[cfg(unix)]
    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A repo with a tracked file, a new one, an ignored one, and a subfolder.
    #[cfg(unix)]
    fn repo(tmp: &Path) -> PathBuf {
        let site = tmp.join("site");
        std::fs::create_dir_all(site.join("src")).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        std::fs::write(site.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(
            site.join("src/auth.rs"),
            "fn login() {\n    let redirect = query;\n    Redirect(redirect)\n}\n",
        )
        .unwrap();
        run_git(&site, &["add", "."]);
        run_git(&site, &["commit", "-q", "-m", "one"]);
        std::fs::write(site.join("notes.md"), "redirect later\n").unwrap();
        std::fs::create_dir(site.join("target")).unwrap();
        std::fs::write(site.join("target/out.txt"), "redirect\n").unwrap();
        site
    }

    #[cfg(unix)]
    #[test]
    fn the_files_are_the_tracked_and_the_new_not_ignored_from_any_folder_in_it() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let (root, mut names, more) = files(&site.join("src")).unwrap();
        names.sort();
        assert_eq!(root, crate::place::resolved(&site));
        assert_eq!(names, [".gitignore", "notes.md", "src/auth.rs"]);
        assert_eq!(more, 0);
        assert_eq!(
            files(tmp.path()).map(|f| f.1),
            Err("not a git repository".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn grep_finds_text_in_any_case_unless_asked_and_new_files_too() {
        let tmp = tempfile::tempdir().unwrap();
        let site = repo(tmp.path());
        let (_, found, more) = grep(&site, "redirect").unwrap();
        let at: Vec<(&str, u32)> = found.iter().map(|m| (m.path.as_str(), m.line)).collect();
        assert_eq!(
            at,
            [("notes.md", 1), ("src/auth.rs", 2), ("src/auth.rs", 3)]
        );
        assert!(!more);
        let (_, found, _) = grep(&site, "Redirect").unwrap();
        assert_eq!(found.len(), 1, "a capital asks for that case");
        let (_, found, _) = grep(&site, "q.ery").unwrap();
        assert!(found.is_empty(), "text, not a pattern");
        let (_, found, _) = grep(&site, "-n").unwrap();
        assert!(found.is_empty(), "a query is never an option");
        for n in 0..MAX_MATCHES + 10 {
            std::fs::write(site.join(format!("f{n}.txt")), "needle\n").unwrap();
        }
        GREP_LINES_READ.with(|n| n.set(0));
        let (_, found, more) = grep(&site, "needle").unwrap();
        assert_eq!((found.len(), more), (MAX_MATCHES, true));
        assert!(
            GREP_LINES_READ.with(|n| n.get()) <= MAX_MATCHES + 1,
            "git is stopped once there are enough"
        );
    }
}
