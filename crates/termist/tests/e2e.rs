#![cfg(unix)]
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use termist_core::*;
use termist_platform::{Client, Paths};
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_termist");

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
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
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
    let mut daemon = Command::new(BIN)
        .arg("daemon")
        .env("TERMIST_HOME", &home)
        .env("TERMIST_CLAUDE_BIN", &agent)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let paths = Paths::under(home.clone());
    let mut c = None;
    for _ in 0..150 {
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
    while daemon.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "daemon did not exit after kill");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
