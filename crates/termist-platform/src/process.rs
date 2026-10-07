//! Short-lived helper processes the daemon runs in the background (a CLI asked for its
//! models): no console window on Windows, and on unix a process group of their own, so
//! one that is given up on is ended together with whatever it started.
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum RunError {
    /// The program is not there.
    NotFound,
    /// It ran past the limit and was killed.
    TimedOut,
    Io(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NotFound => f.write_str("not found"),
            RunError::TimedOut => f.write_str("did not finish in time"),
            RunError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Runs `program args…` with `env` added and `stdin` on its input, and waits at most
/// `limit`. A program that runs over is killed, including its entire process group (on unix).
/// If the program exits before the limit: waits for pipes to close (EOF) until the deadline.
/// If a pipe stays open (grandchildren inherited it): kills the group at the deadline and
/// returns Ok with what was read so far (the program itself finished, its output complete).
/// If the program doesn't finish before the deadline: kills the group and returns Err(TimedOut).
pub fn run(
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
    limit: Duration,
) -> Result<Output, RunError> {
    run_without(program, args, env, &[], stdin, limit)
}

/// `run`, with the variables named in `remove` taken out of what the program inherits.
pub fn run_without(
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    remove: &[&str],
    stdin: Option<&str>,
    limit: Duration,
) -> Result<Output, RunError> {
    run_at(None, program, args, env, remove, stdin, limit)
}

/// `run_without`, in the folder `dir` (ours when `None`).
pub fn run_at(
    dir: Option<&Path>,
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    remove: &[&str],
    stdin: Option<&str>,
    limit: Duration,
) -> Result<Output, RunError> {
    let mut cmd = Command::new(program);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in remove {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    quiet_child(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RunError::NotFound,
        _ => RunError::Io(e),
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_owned();
        // Its own thread: a program that answers before reading all of it must not
        // block us. Dropping the pipe ends the program's input.
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let out_data = Arc::new(Mutex::new(Vec::new()));
    let err_data = Arc::new(Mutex::new(Vec::new()));
    let out_eof = Arc::new(AtomicBool::new(false));
    let err_eof = Arc::new(AtomicBool::new(false));

    drain_to_channel(
        child.stdout.take(),
        Arc::clone(&out_data),
        Arc::clone(&out_eof),
    );
    drain_to_channel(
        child.stderr.take(),
        Arc::clone(&err_data),
        Arc::clone(&err_eof),
    );

    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| {
            kill_quiet_child(&mut child);
            RunError::Io(e)
        })? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_quiet_child(&mut child);
            return Err(RunError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    // Child has exited. Wait for pipes to close (reach EOF) until the deadline.
    while (!out_eof.load(Ordering::Acquire) || !err_eof.load(Ordering::Acquire))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }

    // If deadline passed while pipes still open (grandchildren holding them),
    // kill the rest of the group.
    if Instant::now() >= deadline {
        kill_quiet_child(&mut child);
    }

    let stdout = {
        if let Ok(data) = out_data.lock() {
            String::from_utf8_lossy(&data).into_owned()
        } else {
            String::new()
        }
    };
    let stderr = {
        if let Ok(data) = err_data.lock() {
            String::from_utf8_lossy(&data).into_owned()
        } else {
            String::new()
        }
    };

    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

/// Reads a pipe to its end on a thread, sending chunks to a shared buffer.
/// Signals EOF when the pipe closes (read returns 0).
fn drain_to_channel(
    pipe: Option<impl Read + Send + 'static>,
    data: Arc<Mutex<Vec<u8>>>,
    eof: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut buf = [0u8; 4096];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => {
                        // EOF reached
                        eof.store(true, Ordering::Release);
                        break;
                    }
                    Ok(n) => {
                        // Append chunk
                        if let Ok(mut d) = data.lock() {
                            d.extend_from_slice(&buf[..n]);
                        }
                    }
                    Err(_) => {
                        // Error treated as EOF
                        eof.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        } else {
            // No pipe, signal EOF immediately
            eof.store(true, Ordering::Release);
        }
    });
}

/// Sets up `cmd` to run quietly: on Windows without a console window (the daemon has
/// none, so each child would open its own); on unix in its own process group.
pub fn quiet_child(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
}

/// Ends a child started with `quiet_child` and reaps it: on unix its whole process group
/// (a node shim and the native CLI it started), on Windows the child itself.
pub fn kill_quiet_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) takes no pointers. The group id is our child's pid, and a pid
        // is not reused while a process group of that id still has members, nor while
        // the child is not yet reaped.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: Option<&str>, limit: Duration) -> Result<Output, RunError> {
        run(
            Path::new("/bin/sh"),
            &["-c", script],
            &[("TERMIST_TEST_VALUE", "from env")],
            stdin,
            limit,
        )
    }

    #[test]
    fn output_error_and_status_come_back() {
        let out = sh(
            "echo out; echo err >&2; exit 3",
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            out,
            Output {
                success: false,
                stdout: "out\n".into(),
                stderr: "err\n".into(),
            }
        );
    }

    #[test]
    fn input_and_environment_reach_the_program() {
        let out = sh(
            "cat; echo \" $TERMIST_TEST_VALUE\"",
            Some("piped"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "piped from env\n");
    }

    #[test]
    fn removed_variables_are_not_inherited() {
        let out = run_without(
            Path::new("/bin/sh"),
            &["-c", "printf %s \"${HOME-unset} $TERMIST_TEST_VALUE\""],
            &[("TERMIST_TEST_VALUE", "kept")],
            &["HOME"],
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout, "unset kept");
    }

    #[test]
    fn a_large_output_does_not_block() {
        let out = sh(
            "i=0; while [ $i -lt 20000 ]; do echo 0123456789; i=$((i+1)); done",
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out.stdout.len(), 20_000 * 11);
    }

    #[test]
    fn a_program_that_hangs_is_killed_at_the_limit() {
        let start = Instant::now();
        let err = sh("sleep 5", None, Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, RunError::TimedOut), "{err}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    /// Reads the pid the script wrote, waiting briefly for the file to appear.
    fn read_pid(pidfile: &Path) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(contents) = std::fs::read_to_string(pidfile)
                && let Ok(pid) = contents.trim().parse::<libc::pid_t>()
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "pidfile {} was never written",
                pidfile.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// True when the process is gone or a zombie (killed, not yet reaped).
    fn gone_or_zombie(pid: libc::pid_t) -> bool {
        // SAFETY: kill with signal 0 just tests if the process exists
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // The state follows the last ')' (the command name may contain anything).
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
                .is_some_and(|state| state == 'Z' || state == 'X'),
            Err(_) => false,
        }
    }

    /// Polls up to 2 s for the process to be gone and asserts it is.
    fn assert_gone(pid: libc::pid_t, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone_or_zombie(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone_or_zombie(pid), "{what} (pid {pid}) still alive");
    }

    #[test]
    fn a_grandchild_is_killed_with_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");

        let script = format!("sleep 30 & echo $! > {} && wait", pidfile.display());
        let start = Instant::now();
        let err = sh(&script, None, Duration::from_millis(300)).unwrap_err();
        assert!(
            matches!(err, RunError::TimedOut),
            "timeout expected but got: {err}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "group kill returned promptly"
        );

        let pid = read_pid(&pidfile);
        assert_gone(pid, "grandchild after group kill");
    }

    #[test]
    fn a_child_that_exits_with_a_grandchild_holding_stdout_reads_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("leftover.pid");

        let script = format!("sleep 30 & echo $! > {} && echo out", pidfile.display());
        let start = Instant::now();
        let out = sh(&script, None, Duration::from_secs(1)).unwrap();
        let elapsed = start.elapsed();

        // Verify output was captured despite grandchild holding pipe
        assert_eq!(out.stdout.trim(), "out", "output captured");
        assert!(out.success, "child exited successfully");

        // Verify we returned within reasonable time (wait for EOF + grandchild to be killed)
        assert!(
            elapsed < Duration::from_secs(3),
            "returned within deadline: {}ms",
            elapsed.as_millis()
        );

        let pid = read_pid(&pidfile);
        assert_gone(pid, "leftover grandchild");
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let err = run(
            Path::new("/nonexistent/termist-no-such-program"),
            &[],
            &[],
            None,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, RunError::NotFound), "{err}");
    }

    #[test]
    fn a_quiet_child_leads_its_own_process_group_and_is_killed_with_it() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        quiet_child(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        // SAFETY: getpgid(2) takes no pointers.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        kill_quiet_child(&mut child);
        assert!(child.try_wait().unwrap().is_some(), "ended and reaped");
    }
}
