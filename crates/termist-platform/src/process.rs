//! Short-lived helper processes the daemon runs in the background (a CLI asked for its
//! models): no console window on Windows, and on unix a process group of their own, so
//! one that is given up on is ended together with whatever it started.
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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
/// If the child exits but its group is still running (grandchildren holding stdout/stderr),
/// the reads are bounded by the same deadline. If reads timeout while the group is running,
/// the group is killed and returns what was read (not TimedOut, since the child already exited).
pub fn run(
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
    limit: Duration,
) -> Result<Output, RunError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    drain_to_channel(child.stdout.take(), out_tx);
    drain_to_channel(child.stderr.take(), err_tx);

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

    // Bound reads by the same deadline (child has exited at this point).
    // Use a short timeout (500ms) to wait for the drain threads: if they don't
    // send data by then, they're likely blocked on a grandchild holding the pipe.
    let read_timeout = Duration::from_millis(500);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let effective_timeout = if remaining < read_timeout {
        remaining
    } else {
        read_timeout
    };
    let stdout = out_rx.recv_timeout(effective_timeout).unwrap_or_default();
    let stderr = err_rx.recv_timeout(effective_timeout).unwrap_or_default();

    // If reads timed out (drain threads blocked on grandchildren), kill them.
    // Otherwise wait for any remaining time up to the deadline.
    let now = Instant::now();
    if now >= deadline {
        kill_quiet_child(&mut child);
    }

    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

/// Reads a pipe to its end on a thread, sending the result over a channel,
/// so a full pipe or a grandchild holding it never stalls the program.
fn drain_to_channel(pipe: Option<impl Read + Send + 'static>, tx: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
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

    #[test]
    fn a_grandchild_is_killed_with_the_whole_group() {
        let start = Instant::now();
        let err = sh("sleep 30 & wait", None, Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, RunError::TimedOut), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "group kill ended the grandchild promptly"
        );
    }

    #[test]
    fn a_child_that_exits_with_a_grandchild_holding_stdout_does_not_hang() {
        let start = Instant::now();
        let _out = sh("sleep 30 & echo out", None, Duration::from_secs(2)).unwrap();
        let elapsed = start.elapsed();
        // The limit is 2 seconds; with read_timeout of 500ms we return within ~2.5 seconds
        assert!(
            elapsed < Duration::from_millis(2600),
            "run returns within deadline even when grandchild holds pipe: {}ms",
            elapsed.as_millis()
        );
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
