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

/// Waits for a `SessionUpdated` for `id` whose status differs from `from`, skipping only
/// duplicate broadcasts of that same `from` status in between (PTY activity broadcasts
/// a `SessionUpdated` that repeats the current, unchanged status, which can race with a
/// real transition). Unlike filtering for a specific expected status, this
/// still lets a test catch a wrong transition: the caller asserts on the returned status.
async fn status_change(c: &mut Client, id: SessionId, from: AgentStatus) -> AgentStatus {
    match next_event(
        c,
        |e| matches!(e, ServerEvent::SessionUpdated(s) if s.id == id && s.status != from),
    )
    .await
    {
        ServerEvent::SessionUpdated(s) => s.status,
        _ => unreachable!(),
    }
}

/// A stand-in agent that prints its arguments, then stays alive (or exits when `exit` is true).
fn echo_agent(dir: &std::path::Path, name: &str, exit: bool) -> String {
    let p = dir.join(name);
    let tail = if exit { "exit 0" } else { "sleep 30" };
    std::fs::write(&p, format!("#!/bin/sh\necho \"args: $*\"\n{tail}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.display().to_string()
}

async fn run_daemon(
    paths: &Paths,
    config: DaemonConfig,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let task = tokio::spawn(server::run(paths.clone(), config));
    for _ in 0..250 {
        if Client::connect(paths).await.is_ok() {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon did not come up");
}

async fn shutdown(paths: &Paths, task: tokio::task::JoinHandle<anyhow::Result<()>>) {
    let mut c = Client::connect(paths).await.unwrap();
    c.send(&ClientRequest::Shutdown).await.unwrap();
    next_event(&mut c, |e| *e == ServerEvent::Ack).await;
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn state(c: &mut Client) -> StateSnapshot {
    c.send(&ClientRequest::ListState).await.unwrap();
    match next_event(c, |e| matches!(e, ServerEvent::State(_))).await {
        ServerEvent::State(s) => s,
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
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
    hook(
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest","tool_name":"Write"}"#,
    )
    .await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
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
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::NeedsFeedback).await,
        AgentStatus::Running
    );
    hook("Stop", r#"{"hook_event_name":"Stop"}"#).await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen
    );
    ui.send(&ClientRequest::MarkSeen { session: s.id })
        .await
        .unwrap();
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Unseen).await,
        AgentStatus::Finished
    );
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
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
    hook("PermissionRequest", "{}").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::NeedsFeedback
    );
    hook("Interrupt", "{}").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::NeedsFeedback).await,
        AgentStatus::Finished,
        "Interrupt = cancelled or denied"
    );
}

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
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
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
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen,
        "the child's permission.asked was ignored"
    );
}

// `/new` (or picking another session) in OpenCode moves the card to that session.
#[tokio::test]
async fn an_opencode_card_follows_a_new_or_switched_session() {
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
        r#"{"type":"session.created","properties":{"info":{"id":"ses_1"}}}"#,
    )
    .await;
    hook("chat.message", r#"{"sessionID":"ses_1"}"#).await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
    hook(
        "session.idle",
        r#"{"type":"session.idle","properties":{"sessionID":"ses_1"}}"#,
    )
    .await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen
    );

    // `/new`: a session without a parent
    hook(
        "session.created",
        r#"{"type":"session.created","properties":{"info":{"id":"ses_2"}}}"#,
    )
    .await;
    hook("chat.message", r#"{"sessionID":"ses_2"}"#).await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Unseen).await,
        AgentStatus::Running
    );
    // a subagent of the new session still never moves the card
    hook(
        "session.created",
        r#"{"type":"session.created","properties":{"info":{"id":"ses_child","parentID":"ses_2"}}}"#,
    )
    .await;
    hook(
        "permission.asked",
        r#"{"type":"permission.asked","properties":{"sessionID":"ses_child"}}"#,
    )
    .await;
    hook(
        "session.idle",
        r#"{"type":"session.idle","properties":{"sessionID":"ses_2"}}"#,
    )
    .await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen,
        "the child's permission.asked was ignored"
    );
    let stored = state(&mut ui).await;
    assert_eq!(
        stored.sessions[0].agent_session_id.as_deref(),
        Some("ses_2")
    );

    // switching to an existing session: its first message moves the card there
    hook("chat.message", r#"{"sessionID":"ses_3"}"#).await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Unseen).await,
        AgentStatus::Running
    );
    let stored = state(&mut ui).await;
    assert_eq!(
        stored.sessions[0].agent_session_id.as_deref(),
        Some("ses_3")
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
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
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
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Finished
    );
}

// An interrupt line written while no turn was running (or just before the next prompt)
// belongs to the turn that ended, never to the new one.
#[tokio::test]
async fn a_stale_interrupt_line_does_not_cancel_the_next_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(&transcript, "").unwrap();
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
    let hook = |event: &'static str| {
        let (paths, payload) = (d.paths.clone(), payload.clone());
        async move {
            hook_client::send_hook(&paths, s.id, Harness::Claude, event, payload)
                .await
                .unwrap()
        }
    };
    hook("UserPromptSubmit").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
    hook("Stop").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, b"{\"text\":\"[Request interrupted by user]\"}\n")
        })
        .unwrap();
    hook("UserPromptSubmit").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Unseen).await,
        AgentStatus::Running
    );
    tokio::time::sleep(Duration::from_millis(800)).await; // a transcript poll or more
    hook("Stop").await;
    assert_eq!(
        status_change(&mut ui, s.id, AgentStatus::Running).await,
        AgentStatus::Unseen,
        "the new turn ran to its end"
    );
}

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

// Without its status plugin OpenCode cannot be followed: the daemon still starts,
// and reports OpenCode as unavailable.
#[tokio::test]
async fn a_plugin_that_cannot_be_written_makes_opencode_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().join("home"));
    paths.ensure().unwrap();
    let blocker = paths.opencode_config_dir();
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, "a file where the directory should be").unwrap();
    let task = run_daemon(
        &paths,
        DaemonConfig {
            claude_bin: Some("/bin/sh".into()),
            opencode_bin: Some("/bin/sh".into()),
            ..Default::default()
        },
    )
    .await;
    let mut c = Client::connect(&paths).await.unwrap();
    c.send(&ClientRequest::ListState).await.unwrap();
    match next_event(&mut c, |e| matches!(e, ServerEvent::Harnesses(_))).await {
        ServerEvent::Harnesses(list) => {
            let available = |h| list.iter().find(|i| i.harness == h).unwrap().available;
            assert!(available(Harness::Claude));
            assert!(!available(Harness::OpenCode));
        }
        _ => unreachable!(),
    }
    drop(c);
    shutdown(&paths, task).await;
}

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

// Two daemons racing to start on the same runtime dir must not both bind; exactly
// one wins and the other bails cleanly.
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

// A client of another protocol version is told who refused it, and can still stop
// this daemon with a bare Shutdown (no Hello), so `termist kill` works across versions.
#[tokio::test]
async fn a_client_of_another_version_is_refused_but_can_stop_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    let task = run_daemon(&paths, shell_config()).await;

    let mut old = Client::without_handshake(termist_platform::ipc::connect(&paths).await.unwrap());
    old.send(&ClientRequest::Hello { version: 1 })
        .await
        .unwrap();
    match next_event(&mut old, |_| true).await {
        ServerEvent::Error { message } => assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }

    let mut bare = Client::without_handshake(termist_platform::ipc::connect(&paths).await.unwrap());
    bare.send(&ClientRequest::Shutdown).await.unwrap();
    next_event(&mut bare, |e| *e == ServerEvent::Ack).await;
    timeout(Duration::from_secs(3), task)
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

#[tokio::test]
async fn sessions_survive_a_daemon_restart_and_resume_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().join("home"));
    let config = DaemonConfig {
        claude_bin: Some(echo_agent(tmp.path(), "claude", false)),
        shell: Some("/bin/sh".into()),
        ..Default::default()
    };

    let task = run_daemon(&paths, config.clone()).await;
    let mut c = Client::connect(&paths).await.unwrap();
    let project = add_project(&mut c, tmp.path().to_path_buf()).await;
    let claude = create(
        &mut c,
        project,
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
    // a prompt was sent, so Claude's conversation exists and can be resumed
    hook_client::send_hook(
        &paths,
        claude.id,
        Harness::Claude,
        "UserPromptSubmit",
        "{}".into(),
    )
    .await
    .unwrap();
    assert_eq!(
        status_change(&mut c, claude.id, AgentStatus::Fresh).await,
        AgentStatus::Running
    );
    let shell = create(&mut c, project, SessionKind::Shell).await;
    c.send(&ClientRequest::KillSession { session: shell.id })
        .await
        .unwrap();
    next_event(&mut c, |e| *e == ServerEvent::SessionRemoved(shell.id)).await;
    drop(c);
    shutdown(&paths, task).await;

    let task = run_daemon(&paths, config).await;
    let mut c = Client::connect(&paths).await.unwrap();
    let s = state(&mut c).await;
    assert_eq!(s.projects.len(), 1);
    assert_eq!(
        s.sessions.len(),
        1,
        "the killed shell is gone, the claude session survives"
    );
    let restored = &s.sessions[0];
    assert_eq!(
        (restored.id, restored.status),
        (claude.id, AgentStatus::Disconnected)
    );
    assert_eq!(restored.agent_session_id, claude.agent_session_id);
    assert_eq!(restored.name, claude.name);

    // wide enough that the echoed arguments (a long temp path, then --resume <uuid>) never wrap
    c.send(&ClientRequest::Resume {
        session: claude.id,
        cols: 300,
        rows: 10,
    })
    .await
    .unwrap();
    assert_eq!(
        status_change(&mut c, claude.id, AgentStatus::Disconnected).await,
        AgentStatus::Fresh
    );
    c.send(&ClientRequest::Attach {
        session: claude.id,
        cols: 300,
        rows: 10,
    })
    .await
    .unwrap();
    let sid = claude.agent_session_id.clone().unwrap();
    wait_screen_text(&mut c, claude.id, &format!("--resume {sid}")).await;
    drop(c);
    shutdown(&paths, task).await;
}

/// Waits until `id` has exited (any code), skipping activity broadcasts.
async fn exited(c: &mut Client, id: SessionId) -> AgentStatus {
    match next_event(c, |e| {
        matches!(e, ServerEvent::SessionUpdated(u) if u.id == id && matches!(u.status, AgentStatus::Exited { .. }))
    })
    .await
    {
        ServerEvent::SessionUpdated(u) => u.status,
        _ => unreachable!(),
    }
}

/// Resumes `id`, returns its info once it is back (Fresh), then waits for the stand-in
/// agent to exit and attaches to its last screen.
async fn resume(c: &mut Client, id: SessionId) -> SessionInfo {
    c.send(&ClientRequest::Resume {
        session: id,
        cols: 400,
        rows: 10,
    })
    .await
    .unwrap();
    let back = match next_event(c, |e| {
        matches!(e, ServerEvent::SessionUpdated(u) if u.id == id && u.status == AgentStatus::Fresh)
    })
    .await
    {
        ServerEvent::SessionUpdated(u) => u,
        _ => unreachable!(),
    };
    exited(c, id).await;
    c.send(&ClientRequest::Attach {
        session: id,
        cols: 400,
        rows: 10,
    })
    .await
    .unwrap();
    back
}

// Claude creates its conversation only with the first prompt: before that there is
// nothing for `--resume` to find, so Resume starts a new conversation instead.
#[tokio::test]
async fn a_claude_card_without_a_prompt_resumes_as_a_new_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        claude_bin: Some(echo_agent(tmp.path(), "claude", true)),
        ..Default::default()
    })
    .await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut c, tmp.path().to_path_buf()).await;
    let s = create(
        &mut c,
        project,
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
    let first_id = s.agent_session_id.clone().unwrap();
    exited(&mut c, s.id).await;

    let back = resume(&mut c, s.id).await;
    let new_id = back.agent_session_id.clone().unwrap();
    assert_ne!(new_id, first_id, "a new conversation gets a new id");
    wait_screen_text(&mut c, s.id, &format!("--session-id {new_id}")).await;

    hook_client::send_hook(
        &d.paths,
        s.id,
        Harness::Claude,
        "UserPromptSubmit",
        "{}".into(),
    )
    .await
    .unwrap();
    let back = resume(&mut c, s.id).await;
    assert_eq!(back.agent_session_id.as_deref(), Some(new_id.as_str()));
    wait_screen_text(&mut c, s.id, &format!("--resume {new_id}")).await;
}

#[tokio::test]
async fn resuming_without_a_captured_id_starts_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        codex_bin: Some(echo_agent(tmp.path(), "codex", true)),
        ..Default::default()
    })
    .await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut c, tmp.path().to_path_buf()).await;
    let s = create(
        &mut c,
        project,
        SessionKind::Agent {
            harness: Harness::Codex,
        },
    )
    .await;
    // The echo agent prints output before it exits, and PTY activity may itself
    // broadcast a SessionUpdated before the exit is reported; wait for the
    // first update whose status has moved off Fresh, and check that one instead.
    let exited = match next_event(&mut c, |e| {
        matches!(e, ServerEvent::SessionUpdated(u) if u.id == s.id && u.status != AgentStatus::Fresh)
    })
    .await
    {
        ServerEvent::SessionUpdated(u) => u.status,
        _ => unreachable!(),
    };
    assert_eq!(exited, AgentStatus::Exited { code: Some(0) });
    c.send(&ClientRequest::Resume {
        session: s.id,
        cols: 4000,
        rows: 10,
    })
    .await
    .unwrap();
    assert_eq!(
        status_change(&mut c, s.id, exited).await,
        AgentStatus::Fresh
    );
    c.send(&ClientRequest::Attach {
        session: s.id,
        cols: 4000,
        rows: 10,
    })
    .await
    .unwrap();
    // wide enough that codex's own hook flags (very long under the test binary's own
    // long exe path) never wrap; "args: -c hooks." can only appear if the arguments
    // do not start with `resume <id>`
    wait_screen_text(&mut c, s.id, "args: -c hooks.").await;
}

#[tokio::test]
async fn a_running_session_cannot_be_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(DaemonConfig {
        claude_bin: Some(sleeping_agent(tmp.path())),
        ..Default::default()
    })
    .await;
    let mut c = Client::connect(&d.paths).await.unwrap();
    let project = add_project(&mut c, tmp.path().to_path_buf()).await;
    let s = create(
        &mut c,
        project,
        SessionKind::Agent {
            harness: Harness::Claude,
        },
    )
    .await;
    c.send(&ClientRequest::Resume {
        session: s.id,
        cols: 80,
        rows: 10,
    })
    .await
    .unwrap();
    match next_event(&mut c, |e| matches!(e, ServerEvent::Error { .. })).await {
        ServerEvent::Error { message } => assert!(message.contains("still running"), "{message}"),
        _ => unreachable!(),
    }
}
