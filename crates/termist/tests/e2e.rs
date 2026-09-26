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

/// Copies a fixture script into `dir` as an executable and returns its path.
fn fixture(dir: &std::path::Path, name: &str) -> PathBuf {
    let dest = dir.join(name);
    std::fs::copy(
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")),
        &dest,
    )
    .unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    dest
}

/// Starts `termist daemon` for `home` with extra env vars; returns a connected client and
/// the guard that stops the daemon when the test ends (even on a failed assertion).
async fn daemon(
    home: &std::path::Path,
    envs: &[(&str, &std::path::Path)],
) -> (Paths, Client, DaemonGuard) {
    let mut cmd = Command::new(BIN);
    cmd.arg("daemon")
        .env("TERMIST_HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn().unwrap();
    let guard = DaemonGuard {
        home: home.to_path_buf(),
        child: Some(child),
    };
    let paths = Paths::under(home.to_path_buf());
    for _ in 0..250 {
        if let Ok(c) = Client::connect(&paths).await {
            return (paths, c, guard);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never came up");
}

/// Creates a session of `kind` in `project_dir`, attaches wide, and plays one turn:
/// answers the permission prompt with "y" and returns the status sequence and the screen.
async fn play_turn(
    c: &mut Client,
    project_dir: &std::path::Path,
    kind: SessionKind,
) -> (SessionInfo, Vec<AgentStatus>, Snapshot) {
    c.send(&ClientRequest::AddProject {
        path: project_dir.to_path_buf(),
    })
    .await
    .unwrap();
    let ServerEvent::State(state) = recv_until(c, |e| matches!(e, ServerEvent::State(_))).await
    else {
        unreachable!()
    };
    c.send(&ClientRequest::CreateSession {
        project: state.projects[0].id,
        kind,
        prompt: None,
        cols: 300,
        rows: 20,
    })
    .await
    .unwrap();
    let ServerEvent::SessionUpdated(mut info) =
        recv_until(c, |e| matches!(e, ServerEvent::SessionUpdated(_))).await
    else {
        unreachable!()
    };
    c.send(&ClientRequest::Attach {
        session: info.id,
        cols: 300,
        rows: 20,
    })
    .await
    .unwrap();
    let (mut statuses, mut screen, mut answered) = (vec![], Snapshot::default(), false);
    timeout(Duration::from_secs(15), async {
        loop {
            match c.recv().await.unwrap().unwrap() {
                ServerEvent::SessionUpdated(u) if u.id == info.id => {
                    if statuses.last() != Some(&u.status) && u.status != AgentStatus::Fresh {
                        statuses.push(u.status);
                    }
                    if u.status == AgentStatus::NeedsFeedback && !answered {
                        answered = true;
                        c.send(&ClientRequest::Input {
                            session: u.id,
                            data: b"y\r".to_vec(),
                        })
                        .await
                        .unwrap();
                    }
                    let done = u.status == AgentStatus::Unseen;
                    info = u;
                    if done {
                        break;
                    }
                }
                ServerEvent::Screen { session, update } if session == info.id => {
                    screen.apply(&update)
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the turn never finished");
    recv_until(c, |e| {
        if let ServerEvent::Screen { update, .. } = e {
            screen.apply(update);
        }
        (0..screen.rows as usize).any(|r| screen.line_text(r).contains("done: y"))
    })
    .await;
    (info, statuses, screen)
}

fn screen_text(s: &Snapshot) -> String {
    (0..s.rows as usize)
        .map(|r| s.line_text(r) + "\n")
        .collect()
}

#[tokio::test]
async fn a_fake_claude_turn_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = fixture(tmp.path(), "fake-claude.sh");
    let home = tmp.path().join("home");
    let (_paths, mut c, mut guard) = daemon(&home, &[("TERMIST_CLAUDE_BIN", &agent)]).await;

    let (_info, statuses, screen) = play_turn(
        &mut c,
        tmp.path(),
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
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
    let text = screen_text(&screen);
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

#[tokio::test]
async fn a_codex_turn_through_its_real_hook_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fixture(tmp.path(), "fake-codex.sh");
    let (_paths, mut c, _guard) =
        daemon(&tmp.path().join("home"), &[("TERMIST_CODEX_BIN", &bin)]).await;
    let (info, statuses, screen) = play_turn(
        &mut c,
        tmp.path(),
        SessionKind::Agent {
            harness: Harness::Codex,
        },
    )
    .await;
    assert_eq!(
        statuses,
        vec![
            AgentStatus::Running,
            AgentStatus::NeedsFeedback,
            AgentStatus::Running,
            AgentStatus::Unseen
        ]
    );
    assert_eq!(info.agent_session_id.as_deref(), Some("codex-e2e-1"));
    let text = screen_text(&screen);
    assert!(
        text.contains("hooks.state="),
        "trust entries are passed: {text}"
    );
    assert!(!text.contains("dangerously"));
}

#[tokio::test]
async fn an_opencode_turn_ignores_its_subagent() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fixture(tmp.path(), "fake-opencode.sh");
    let (_paths, mut c, _guard) =
        daemon(&tmp.path().join("home"), &[("TERMIST_OPENCODE_BIN", &bin)]).await;
    let (info, statuses, screen) = play_turn(
        &mut c,
        tmp.path(),
        SessionKind::Agent {
            harness: Harness::OpenCode,
        },
    )
    .await;
    assert_eq!(
        statuses,
        vec![
            AgentStatus::Running,
            AgentStatus::NeedsFeedback,
            AgentStatus::Running,
            AgentStatus::Unseen
        ]
    );
    assert_eq!(info.agent_session_id.as_deref(), Some("ses_parent"));
    assert!(
        screen_text(&screen).contains("plugin: ok"),
        "the plugin was written into OPENCODE_CONFIG_DIR"
    );
}

#[tokio::test]
async fn a_claude_session_survives_kill_and_restart_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fixture(tmp.path(), "fake-claude.sh");
    let home = tmp.path().join("home");
    let session = {
        let (_paths, mut c, _guard) = daemon(&home, &[("TERMIST_CLAUDE_BIN", &bin)]).await;
        let (info, _, _) = play_turn(
            &mut c,
            tmp.path(),
            SessionKind::Agent {
                harness: Harness::Claude,
            },
        )
        .await;
        assert_eq!(info.agent_session_id.as_deref(), Some("fake-session"));
        info
        // _guard drops here: `termist kill`, the daemon exits, the record stays
    };
    let (_paths, mut c, _guard) = daemon(&home, &[("TERMIST_CLAUDE_BIN", &bin)]).await;
    c.send(&ClientRequest::ListState).await.unwrap();
    let ServerEvent::State(state) =
        recv_until(&mut c, |e| matches!(e, ServerEvent::State(_))).await
    else {
        unreachable!()
    };
    let restored = state
        .sessions
        .iter()
        .find(|s| s.id == session.id)
        .expect("the session was stored");
    assert_eq!(restored.status, AgentStatus::Disconnected);
    assert_eq!(restored.agent_session_id.as_deref(), Some("fake-session"));
    c.send(&ClientRequest::Resume {
        session: session.id,
        cols: 300,
        rows: 20,
    })
    .await
    .unwrap();
    c.send(&ClientRequest::Attach {
        session: session.id,
        cols: 300,
        rows: 20,
    })
    .await
    .unwrap();
    let mut screen = Snapshot::default();
    recv_until(&mut c, |e| {
        if let ServerEvent::Screen { session: s, update } = e
            && *s == session.id
        {
            screen.apply(update);
        }
        screen_text(&screen).contains("--resume fake-session")
    })
    .await;
}

/// Kills the TUI running in the test PTY when dropped.
struct KillOnDrop(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// The input thread used to start before the keyboard-enhancement
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
