#![cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;
use termist_core::*;
use termist_daemon::hook_client;
use termist_daemon::launch::DaemonConfig;
use termist_daemon::server;
use termist_platform::{Client, Paths};
use tokio::time::timeout;

struct Daemon {
    paths: Paths,
    // Kept alive so the daemon task isn't dropped/aborted; only read in one test.
    _task: tokio::task::JoinHandle<anyhow::Result<()>>,
    _tmp: tempfile::TempDir,
}

async fn start(config: DaemonConfig) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    let task = tokio::spawn(server::run(paths.clone(), config));
    for _ in 0..250 {
        if Client::connect(&paths).await.is_ok() {
            return Daemon {
                paths,
                _task: task,
                _tmp: tmp,
            };
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon did not come up");
}

fn shell_config() -> DaemonConfig {
    DaemonConfig {
        shell: Some("/bin/sh".into()),
        ..Default::default()
    }
}

/// A stand-in `claude` that ignores its arguments and stays alive.
fn sleeping_agent(dir: &std::path::Path) -> String {
    let p = dir.join("sleepy-agent.sh");
    std::fs::write(&p, "#!/bin/sh\nsleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.display().to_string()
}

async fn next_event(c: &mut Client, pred: impl Fn(&ServerEvent) -> bool) -> ServerEvent {
    timeout(Duration::from_secs(5), async {
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
    .expect("timed out waiting for event")
}

async fn add_project(c: &mut Client, dir: PathBuf) -> ProjectId {
    c.send(&ClientRequest::AddProject { path: dir })
        .await
        .unwrap();
    match next_event(c, |e| matches!(e, ServerEvent::State(_))).await {
        ServerEvent::State(s) => s.projects[0].id,
        _ => unreachable!(),
    }
}

async fn create(c: &mut Client, project: ProjectId, kind: SessionKind) -> SessionInfo {
    c.send(&ClientRequest::CreateSession {
        project,
        kind,
        prompt: None,
        cols: 60,
        rows: 10,
    })
    .await
    .unwrap();
    match next_event(c, |e| {
        matches!(
            e,
            ServerEvent::SessionUpdated(_) | ServerEvent::Error { .. }
        )
    })
    .await
    {
        ServerEvent::SessionUpdated(s) => s,
        other => panic!("{other:?}"),
    }
}

async fn wait_screen_text(c: &mut Client, id: SessionId, needle: &str) -> ScreenUpdate {
    let mut screen = Snapshot::default();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(ServerEvent::Screen { session, update }) = c.recv().await.unwrap()
                && session == id
            {
                screen.apply(&update);
                if (0..screen.rows as usize).any(|r| screen.line_text(r).contains(needle)) {
                    return update;
                }
            }
        }
    })
    .await
    .expect("text never appeared")
}

async fn status_update(c: &mut Client, id: SessionId) -> AgentStatus {
    match next_event(
        c,
        |e| matches!(e, ServerEvent::SessionUpdated(s) if s.id == id),
    )
    .await
    {
        ServerEvent::SessionUpdated(s) => s.status,
        _ => unreachable!(),
    }
}

async fn info_update(c: &mut Client, id: SessionId) -> SessionInfo {
    match next_event(
        c,
        |e| matches!(e, ServerEvent::SessionUpdated(s) if s.id == id),
    )
    .await
    {
        ServerEvent::SessionUpdated(s) => s,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn a_shell_session_round_trip() {
    let d = start(shell_config()).await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut c, d._tmp.path().to_path_buf()).await;
    let s = create(&mut c, project, SessionKind::Shell).await;
    assert_eq!(s.name, "shell-1");
    c.send(&ClientRequest::Attach {
        session: s.id,
        cols: 60,
        rows: 10,
    })
    .await
    .unwrap();
    c.send(&ClientRequest::Input {
        session: s.id,
        data: b"echo termist-$((40+2))\r".to_vec(),
    })
    .await
    .unwrap();
    wait_screen_text(&mut c, s.id, "termist-42").await;
    c.send(&ClientRequest::KillSession { session: s.id })
        .await
        .unwrap();
    next_event(&mut c, |e| *e == ServerEvent::SessionRemoved(s.id)).await;
}

#[tokio::test]
async fn claude_hooks_drive_the_status_dot() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        shell: None,
        claude_bin: Some(sleeping_agent(tmp.path())),
        ..Default::default()
    })
    .await;
    let mut ui = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut ui, tmp.path().to_path_buf()).await;
    let s = create(
        &mut ui,
        project,
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
    assert_eq!(s.status, AgentStatus::Fresh);
    assert!(s.agent_session_id.is_some());

    let hook = |event: &'static str, payload: &'static str| {
        let paths = d.paths.clone();
        async move {
            hook_client::send_hook(&paths, s.id, Harness::Claude, event, payload.into())
                .await
                .unwrap()
        }
    };
    hook(
        "UserPromptSubmit",
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    )
    .await;
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Running);
    hook(
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest","tool_name":"Write"}"#,
    )
    .await;
    assert_eq!(
        status_update(&mut ui, s.id).await,
        AgentStatus::NeedsFeedback
    );
    // an idle notification must not produce an update; typing then answers optimistically
    hook("Notification", r#"{"notification_type":"idle_prompt"}"#).await;
    ui.send(&ClientRequest::Input {
        session: s.id,
        data: b"y".to_vec(),
    })
    .await
    .unwrap();
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Running);
    hook("Stop", r#"{"hook_event_name":"Stop"}"#).await;
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Unseen);
    ui.send(&ClientRequest::MarkSeen { session: s.id })
        .await
        .unwrap();
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Finished);
}

#[tokio::test]
async fn codex_hooks_drive_status_and_capture_its_session_id() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        codex_bin: Some(sleeping_agent(tmp.path())),
        ..Default::default()
    })
    .await;
    let mut ui = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut ui, tmp.path().to_path_buf()).await;
    let s = create(
        &mut ui,
        project,
        SessionKind::Agent {
            harness: Harness::Codex,
        },
    )
    .await;
    assert_eq!(s.agent_session_id, None);
    let hook = |event: &'static str, payload: &'static str| {
        let paths = d.paths.clone();
        async move {
            hook_client::send_hook(&paths, s.id, Harness::Codex, event, payload.into())
                .await
                .unwrap()
        }
    };
    hook(
        "SessionStart",
        r#"{"session_id":"019a-codex","source":"startup"}"#,
    )
    .await;
    assert_eq!(
        info_update(&mut ui, s.id).await.agent_session_id.as_deref(),
        Some("019a-codex")
    );
    hook("UserPromptSubmit", "{}").await;
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Running);
    hook("PermissionRequest", "{}").await;
    assert_eq!(
        status_update(&mut ui, s.id).await,
        AgentStatus::NeedsFeedback
    );
    hook("Interrupt", "{}").await;
    assert_eq!(
        status_update(&mut ui, s.id).await,
        AgentStatus::Finished,
        "Interrupt = cancelled or denied"
    );
}

// Review Focus 3
#[tokio::test]
async fn opencode_subagent_events_do_not_move_the_parent_card() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        opencode_bin: Some(sleeping_agent(tmp.path())),
        ..Default::default()
    })
    .await;
    let mut ui = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut ui, tmp.path().to_path_buf()).await;
    let s = create(
        &mut ui,
        project,
        SessionKind::Agent {
            harness: Harness::OpenCode,
        },
    )
    .await;
    let hook = |event: &'static str, payload: &'static str| {
        let paths = d.paths.clone();
        async move {
            hook_client::send_hook(&paths, s.id, Harness::OpenCode, event, payload.into())
                .await
                .unwrap()
        }
    };
    hook(
        "session.created",
        r#"{"type":"session.created","properties":{"info":{"id":"ses_parent"}}}"#,
    )
    .await;
    assert_eq!(
        info_update(&mut ui, s.id).await.agent_session_id.as_deref(),
        Some("ses_parent")
    );
    hook(
        "session.created",
        r#"{"type":"session.created","properties":{"info":{"id":"ses_child","parentID":"ses_parent"}}}"#,
    )
    .await;
    hook("chat.message", r#"{"sessionID":"ses_parent"}"#).await;
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Running);
    // a subagent asks for permission: the parent card must not turn red
    hook(
        "permission.asked",
        r#"{"type":"permission.asked","properties":{"sessionID":"ses_child"}}"#,
    )
    .await;
    hook(
        "session.idle",
        r#"{"type":"session.idle","properties":{"sessionID":"ses_parent"}}"#,
    )
    .await;
    assert_eq!(
        status_update(&mut ui, s.id).await,
        AgentStatus::Unseen,
        "the child's permission.asked was ignored"
    );
}

#[tokio::test]
async fn a_cancelled_claude_turn_is_read_from_its_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        "{\"text\":\"[Request interrupted by user]\"}\n",
    )
    .unwrap(); // old history
    let d = start(DaemonConfig {
        claude_bin: Some(sleeping_agent(tmp.path())),
        ..Default::default()
    })
    .await;
    let mut ui = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut ui, tmp.path().to_path_buf()).await;
    let s = create(
        &mut ui,
        project,
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
    let payload = serde_json::json!({ "transcript_path": transcript }).to_string();
    hook_client::send_hook(&d.paths, s.id, Harness::Claude, "UserPromptSubmit", payload)
        .await
        .unwrap();
    assert_eq!(
        status_update(&mut ui, s.id).await,
        AgentStatus::Running,
        "history must not cancel"
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .and_then(|mut f| {
            std::io::Write::write_all(
                &mut f,
                b"{\"text\":\"[Request interrupted by user for tool use]\"}\n",
            )
        })
        .unwrap();
    assert_eq!(status_update(&mut ui, s.id).await, AgentStatus::Finished);
}

// Review Focus 3
#[tokio::test]
async fn a_missing_agent_cli_is_an_error_not_a_crash() {
    let d = start(DaemonConfig {
        shell: None,
        claude_bin: Some("/nonexistent/claude".into()),
        ..Default::default()
    })
    .await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut c, d._tmp.path().to_path_buf()).await;
    c.send(&ClientRequest::CreateSession {
        project,
        kind: SessionKind::Agent {
            harness: Harness::Claude,
        },
        prompt: None,
        cols: 60,
        rows: 10,
    })
    .await
    .unwrap();
    match next_event(&mut c, |e| {
        matches!(
            e,
            ServerEvent::Error { .. } | ServerEvent::SessionUpdated(_)
        )
    })
    .await
    {
        ServerEvent::Error { message } => assert!(message.contains("claude"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
    c.send(&ClientRequest::ListState).await.unwrap();
    next_event(
        &mut c,
        |e| matches!(e, ServerEvent::State(s) if s.sessions.is_empty()),
    )
    .await;
}

#[tokio::test]
async fn list_state_is_followed_by_harness_availability() {
    let d = start(DaemonConfig {
        claude_bin: Some("/bin/sh".into()),
        ..Default::default()
    })
    .await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    c.send(&ClientRequest::ListState).await.unwrap();
    next_event(&mut c, |e| matches!(e, ServerEvent::State(_))).await;
    match next_event(&mut c, |e| matches!(e, ServerEvent::Harnesses(_))).await {
        ServerEvent::Harnesses(list) => {
            assert_eq!(
                list.iter().map(|h| h.harness).collect::<Vec<_>>(),
                Harness::ALL.to_vec()
            );
            assert!(list[0].available, "a configured claude binary is available");
        }
        _ => unreachable!(),
    }
}

// Review Focus 4
#[tokio::test]
async fn reattaching_after_a_dropped_client_gets_the_whole_screen() {
    let d = start(shell_config()).await;
    let id = {
        let mut a = Client::connect(&d.paths).await.unwrap();
        let project = add_project(&mut a, d._tmp.path().to_path_buf()).await;
        let s = create(&mut a, project, SessionKind::Shell).await;
        a.send(&ClientRequest::Attach {
            session: s.id,
            cols: 60,
            rows: 10,
        })
        .await
        .unwrap();
        a.send(&ClientRequest::Input {
            session: s.id,
            data: b"echo marker-$((6*7))\r".to_vec(),
        })
        .await
        .unwrap();
        wait_screen_text(&mut a, s.id, "marker-42").await;
        s.id
    }; // client A dropped here without detaching
    let mut b = Client::connect(&d.paths).await.unwrap();
    b.send(&ClientRequest::Attach {
        session: id,
        cols: 60,
        rows: 10,
    })
    .await
    .unwrap();
    let first = next_event(&mut b, |e| matches!(e, ServerEvent::Screen { .. })).await;
    let ServerEvent::Screen { update, .. } = first else {
        unreachable!()
    };
    assert_eq!(
        update.changed.len(),
        10,
        "first update after attach must carry every row"
    );
    let mut screen = Snapshot::default();
    screen.apply(&update);
    assert!((0..10).any(|r| screen.line_text(r).contains("marker-42")));
}

// Review Focus 1
#[tokio::test]
async fn a_second_daemon_refuses_and_a_stale_socket_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    paths.ensure().unwrap();
    std::fs::write(paths.socket_path(), b"stale").unwrap();
    let first = tokio::spawn(server::run(paths.clone(), shell_config()));
    let mut up = false;
    for _ in 0..250 {
        if Client::connect(&paths).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(up, "a stale socket file must not block startup");
    let err = server::run(paths.clone(), shell_config())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");
    let mut c = Client::connect(&paths).await.unwrap();
    c.send(&ClientRequest::Shutdown).await.unwrap();
    next_event(&mut c, |e| *e == ServerEvent::Ack).await;
    timeout(Duration::from_secs(3), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

// Review fix round 1: two daemons racing to start on the same runtime dir must not
// both bind; exactly one wins and the other bails cleanly.
#[tokio::test]
async fn two_daemons_started_together_leave_exactly_one() {
    enum Loser {
        First,
        Second,
    }

    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    let mut t1 = tokio::spawn(server::run(paths.clone(), shell_config()));
    let mut t2 = tokio::spawn(server::run(paths.clone(), shell_config()));

    // Whichever of the two resolves within 3s must be the loser: the winner keeps
    // running (accepting connections) until it is told to shut down.
    let (loser, result) = timeout(Duration::from_secs(3), async {
        tokio::select! {
            r = &mut t1 => (Loser::First, r.unwrap()),
            r = &mut t2 => (Loser::Second, r.unwrap()),
        }
    })
    .await
    .expect("one of the two daemons should have failed within 3s");
    let err = result.unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");

    let mut c = Client::connect(&paths).await.unwrap();
    c.send(&ClientRequest::Shutdown).await.unwrap();
    next_event(&mut c, |e| *e == ServerEvent::Ack).await;

    let survivor = match loser {
        Loser::First => t2,
        Loser::Second => t1,
    };
    timeout(Duration::from_secs(3), survivor)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn run_hook_gives_up_quietly_without_a_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    let started = std::time::Instant::now();
    let ok = hook_client::run_hook(
        &paths,
        "claude",
        "Stop",
        "{}".into(),
        Some(SessionId::new().to_string()),
        Duration::from_secs(2),
    )
    .await;
    assert!(!ok);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        !hook_client::run_hook(
            &paths,
            "claude",
            "Stop",
            "{}".into(),
            None,
            Duration::from_secs(2)
        )
        .await
    );
}
