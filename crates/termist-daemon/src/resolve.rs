//! Finding agent CLIs. A daemon started from a GUI launcher or launchd can have a much
//! shorter PATH than the user's interactive shell, so after PATH we ask the login shell.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Looks `name` up in the directories of a PATH-style variable.
pub fn find_in_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .flat_map(|dir| candidates(&dir, name))
        .find(|p| is_executable(p))
}

/// PATH first; on unix, then the login shell. `None` when neither knows `name`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|p| find_in_path(name, &p))
        .or_else(|| login_shell_lookup(name))
}

#[cfg(unix)]
fn login_shell_lookup(name: &str) -> Option<PathBuf> {
    via_login_shell(name, Duration::from_secs(3))
}

#[cfg(windows)]
fn login_shell_lookup(_name: &str) -> Option<PathBuf> {
    None
}

/// `$SHELL -lc 'command -v NAME'`, killed after `limit`. Only plain names are allowed:
/// the name is interpolated into a shell command.
#[cfg(unix)]
pub fn via_login_shell(name: &str, limit: Duration) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    ask_shell(&shell, name, limit)
}

#[cfg(unix)]
fn ask_shell(shell: &str, name: &str, limit: Duration) -> Option<PathBuf> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut child = match Command::new(shell)
        .args(["-lc", &format!("command -v {name}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // its own process group, so a timeout also ends whatever the profile started
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(shell, name, error = %e, "could not start the login shell for a lookup");
            return None;
        }
    };
    let started = Instant::now();
    // A job the profile started in the background can hold stdout open long after the
    // shell exits, so the read happens on its own thread and is given up at the limit.
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = tx.send(stdout.read_to_string(&mut out).map(|_| out));
    });
    let out = rx.recv_timeout(limit);
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => break false,
        }
    };
    if out.is_err() || !exited {
        // SAFETY: kill(2) takes no pointers. The group id is our child's pid, and a pid
        // is not reused while a process group of that id still has members.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.wait();
    }
    let out = match out {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            tracing::debug!(shell, name, error = %e, "login shell output unreadable");
            return None;
        }
        Err(_) => {
            tracing::warn!(shell, name, ?limit, "login shell lookup timed out");
            return None;
        }
    };
    out.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .rfind(|p| is_executable(p))
}

#[cfg(unix)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    windows_names(name, &exts)
        .into_iter()
        .map(|n| dir.join(n))
        .collect()
}

/// The file names Windows can start for `name`: its PATHEXT variants only, unless it
/// already has an extension. npm puts an extensionless sh script (`codex`) next to
/// `codex.cmd`, and a console can't run the script.
#[cfg(any(windows, test))]
fn windows_names(name: &str, pathext: &str) -> Vec<String> {
    if Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    pathext
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| format!("{name}{e}"))
        .collect()
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn finds_executables_on_a_path_and_skips_plain_files() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        touch(a.path(), "codex", 0o644); // not executable: must be skipped
        touch(b.path(), "codex", 0o755);
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert_eq!(find_in_path("codex", &path), Some(b.path().join("codex")));
        assert_eq!(find_in_path("opencode", &path), None);
    }

    // A profile that starts a background job keeps the shell's stdout open after the
    // shell exits: the lookup must still end at its limit, and take the job with it.
    #[test]
    fn a_login_shell_that_leaves_a_job_behind_is_bounded_and_cleaned_up() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("job.pid");
        let shell = tmp.path().join("fake-shell");
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\nsleep 10 &\necho $! > '{}'\necho /bin/sh\n",
                pid_file.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let found = ask_shell(shell.to_str().unwrap(), "sh", Duration::from_millis(500));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(found, None, "no answer within the limit");
        let job: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only checks that the process exists.
            (unsafe { libc::kill(job, 0) }) != 0
        });
        assert!(gone, "the background job was killed with the shell");
    }

    #[test]
    fn the_login_shell_can_find_a_program_and_refuses_odd_names() {
        let found =
            via_login_shell("sh", std::time::Duration::from_secs(3)).expect("sh via login shell");
        assert!(found.is_absolute());
        assert_eq!(
            via_login_shell("sh; rm -rf /", std::time::Duration::from_secs(3)),
            None
        );
    }
}

#[cfg(test)]
mod windows_tests {
    use super::windows_names;

    #[test]
    fn windows_tries_only_pathext_variants_of_a_bare_name() {
        assert_eq!(
            windows_names("codex", ".COM;.EXE;;.CMD"),
            ["codex.COM", "codex.EXE", "codex.CMD"]
        );
        assert_eq!(windows_names("codex.cmd", ".EXE;.CMD"), ["codex.cmd"]);
    }
}
