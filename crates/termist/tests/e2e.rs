#![cfg(unix)]
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use termist_core::*;
use termist_platform::{Client, Paths};
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_termist");

/// Stops the test's daemon when dropped, so a failed assertion never leaks one:
/// runs `termist kill` for the test's `TERMIST_HOME`, then kills `child` if any.
struct DaemonGuard {
    home: PathBuf,
    child: Option<Child>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::new(BIN)
            .arg("kill")
            .env("TERMIST_HOME", &self.home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// Review Focus 2
#[test]
fn a_hook_without_a_daemon_exits_zero_quickly() {
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut child = Command::new(BIN)
        .args(["hook", "--harness", "claude", "Stop"])
        .env("TERMIST_HOME", tmp.path())
        .env("TERMIST_SESSION_ID", SessionId::new().to_string())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_hook_whose_stdin_never_closes_still_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut child = Command::new(BIN)
        .args(["hook", "--harness", "claude", "Stop"])
        .env("TERMIST_HOME", tmp.path())
        .env("TERMIST_SESSION_ID", SessionId::new().to_string())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let _keep_open = child.stdin.take();
    assert!(child.wait().unwrap().success());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn bare_termist_autostarts_a_detached_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let _guard = DaemonGuard {
        home: home.clone(),
        child: None,
    };

    // No TTY: bare `termist` must fail fast rather than panic, but the daemon it
    // autostarted along the way must keep running after this parent exits.
    let started = Instant::now();
    let output = Command::new(BIN)
        .env("TERMIST_HOME", &home)
        .current_dir(&project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("interactive terminal"),
        "stderr was: {stderr}"
    );

    let paths = Paths::under(home.clone());
    let mut c = None;
    for _ in 0..250 {
        if let Ok(client) = Client::connect(&paths).await {
            c = Some(client);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut c = c.expect("the autostarted daemon never came up; see <home>/data/daemon.log");

    c.send(&ClientRequest::ListState).await.unwrap();
    let ServerEvent::State(state) =
        recv_until(&mut c, |e| matches!(e, ServerEvent::State(_))).await
    else {
        unreachable!()
    };
    let want = std::fs::canonicalize(&project_dir).unwrap();
    assert!(
        state.projects.iter().any(|p| p.path == want),
        "expected a project at {want:?}, got {:?}",
        state.projects
    );
    drop(c);

    let out = Command::new(BIN)
        .arg("kill")
        .env("TERMIST_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Client::connect(&paths).await.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon still accepted connections after kill"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn recv_until(c: &mut Client, mut pred: impl FnMut(&ServerEvent) -> bool) -> ServerEvent {
    timeout(Duration::from_secs(10), async {
        loop {
            let ev = c
                .recv()
                .await
                .unwrap()
                .expect("daemon closed the connection");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .expect("timed out")
}

#[tokio::test]
async fn a_fake_claude_turn_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("fake-claude.sh");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude.sh"),
        &agent,
    )
    .unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = tmp.path().join("home");
    let mut guard = DaemonGuard {
        home: home.clone(),
        child: Some(
            Command::new(BIN)
                .arg("daemon")
                .env("TERMIST_HOME", &home)
                .env("TERMIST_CLAUDE_BIN", &agent)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        ),
    };
    let paths = Paths::under(home.clone());
    let mut c = None;
    for _ in 0..250 {
        if let Ok(client) = Client::connect(&paths).await {
            c = Some(client);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut c = c.expect("daemon never came up");

    c.send(&ClientRequest::AddProject {
        path: tmp.path().to_path_buf(),
    })
    .await
    .unwrap();
    let ServerEvent::State(state) =
        recv_until(&mut c, |e| matches!(e, ServerEvent::State(_))).await
    else {
        unreachable!()
    };
    let project = state.projects[0].id;
    c.send(&ClientRequest::CreateSession {
        project,
        kind: SessionKind::Agent {
            harness: Harness::Claude,
        },
        prompt: None,
        cols: 100,
        rows: 12,
    })
    .await
    .unwrap();
    let ServerEvent::SessionUpdated(s) =
        recv_until(&mut c, |e| matches!(e, ServerEvent::SessionUpdated(_))).await
    else {
        unreachable!()
    };
    c.send(&ClientRequest::Attach {
        session: s.id,
        cols: 100,
        rows: 12,
    })
    .await
    .unwrap();

    let mut statuses = vec![];
    let mut screen = Snapshot::default();
    let mut answered = false;
    timeout(Duration::from_secs(15), async {
        loop {
            match c.recv().await.unwrap().unwrap() {
                ServerEvent::SessionUpdated(info) if info.id == s.id => {
                    if statuses.last() != Some(&info.status) {
                        statuses.push(info.status);
                    }
                    if info.status == AgentStatus::NeedsFeedback && !answered {
                        answered = true;
                        c.send(&ClientRequest::Input {
                            session: s.id,
                            data: b"y\r".to_vec(),
                        })
                        .await
                        .unwrap();
                    }
                    if info.status == AgentStatus::Unseen {
                        break;
                    }
                }
                ServerEvent::Screen { session, update } if session == s.id => screen.apply(&update),
                _ => {}
            }
        }
    })
    .await
    .expect("turn never finished");
    assert_eq!(
        statuses,
        vec![
            AgentStatus::Running,
            AgentStatus::NeedsFeedback,
            AgentStatus::Running,
            AgentStatus::Unseen
        ]
    );

    // the pane shows the launch arguments and the answer
    recv_until(&mut c, |e| {
        if let ServerEvent::Screen { update, .. } = e {
            screen.apply(update);
        }
        (0..screen.rows as usize).any(|r| screen.line_text(r).contains("done: y"))
    })
    .await;
    let text: String = (0..screen.rows as usize)
        .map(|r| screen.line_text(r) + "\n")
        .collect();
    assert!(text.contains("--session-id"), "{text}");
    assert!(text.contains("--settings"), "{text}");

    let out = Command::new(BIN)
        .arg("kill")
        .env("TERMIST_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    let daemon = guard.child.as_mut().unwrap();
    while daemon.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "daemon did not exit after kill");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Kills the TUI running in the test PTY when dropped.
struct KillOnDrop(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Final review F1: the input thread used to start before the keyboard-enhancement
// query and hold crossterm's event-reader lock, so the query timed out after 2 s: the
// first frame came 2 s late and DISAMBIGUATE_ESCAPE_CODES was never pushed.
#[test]
fn the_tui_draws_its_first_frame_at_once_and_enables_keyboard_enhancement() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read;
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let _daemon = DaemonGuard {
        home: home.clone(),
        child: None,
    };

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(BIN);
    cmd.cwd(&project_dir);
    cmd.env("TERMIST_HOME", &home);
    cmd.env("TERM", "xterm-256color");
    let spawned = Instant::now();
    let _tui = KillOnDrop(pair.slave.spawn_command(cmd).unwrap());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();

    // Plays a terminal that supports the kitty keyboard protocol: answers the flags
    // query (ESC[?u) and the primary device attributes query (ESC[c), and reports
    // when the first frame and the keyboard-enhancement push show up.
    let (seen_tx, seen_rx) = mpsc::channel::<(&'static str, Instant)>();
    std::thread::spawn(move || {
        let mut out: Vec<u8> = Vec::new();
        let mut scanned = 0;
        let mut buf = [0u8; 4096];
        let (mut frame, mut pushed) = (false, false);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            const QUERIES: [(&[u8], &[u8]); 2] =
                [(b"\x1b[?u", b"\x1b[?0u"), (b"\x1b[c", b"\x1b[?62;22c")];
            let mut i = scanned;
            while i < out.len() {
                let rest = &out[i..];
                // a query split across reads: wait for the rest of it
                if QUERIES
                    .iter()
                    .any(|(q, _)| rest.len() < q.len() && q.starts_with(rest))
                {
                    break;
                }
                for (query, answer) in QUERIES {
                    if rest.starts_with(query) {
                        let _ = writer.write_all(answer);
                    }
                }
                i += 1;
            }
            let _ = writer.flush();
            scanned = i;
            let text = String::from_utf8_lossy(&out);
            if !frame && text.contains(" termist ") {
                frame = true;
                let _ = seen_tx.send(("frame", Instant::now()));
            }
            if !pushed && text.contains("\x1b[>1u") {
                pushed = true;
                let _ = seen_tx.send(("push", Instant::now()));
            }
        }
    });

    let mut seen = std::collections::HashMap::new();
    while seen.len() < 2 {
        match seen_rx.recv_timeout(Duration::from_secs(5)) {
            Ok((what, at)) => {
                seen.insert(what, at - spawned);
            }
            Err(_) => break,
        }
    }
    let frame = seen.get("frame").copied();
    assert!(
        frame.is_some_and(|d| d < Duration::from_secs(1)),
        "first frame after {frame:?} (want < 1 s)"
    );
    assert!(
        seen.contains_key("push"),
        "DISAMBIGUATE_ESCAPE_CODES (ESC[>1u) was never pushed; saw {seen:?}"
    );
}
