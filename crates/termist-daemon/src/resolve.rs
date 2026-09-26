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
    use std::io::Read;
    use std::process::{Command, Stdio};
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut child = Command::new(shell)
        .args(["-lc", &format!("command -v {name}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
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
