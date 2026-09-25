use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use termist_core::env::should_scrub;
use termist_core::screen::diff;
use termist_core::{ServerEvent, SessionId, Snapshot};
use termist_term::{TermConfig, TermCore, TermEvent};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub id: SessionId,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

pub enum SessionCmd {
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    Attach {
        client: ClientId,
        out: UnboundedSender<ServerEvent>,
    },
    Detach {
        client: ClientId,
    },
    Kill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionNote {
    Title(SessionId, Option<String>),
    Activity(SessionId),
    Exited(SessionId, Option<i32>),
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

pub fn spawn(
    spec: SpawnSpec,
    notes: UnboundedSender<SessionNote>,
) -> anyhow::Result<UnboundedSender<SessionCmd>> {
    let pair = native_pty_system().openpty(pty_size(spec.cols, spec.rows))?;
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(&spec.args);
    cmd.cwd(&spec.cwd);
    for (key, _) in std::env::vars_os() {
        if let Some(k) = key.to_str()
            && should_scrub(k)
        {
            cmd.env_remove(k);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    // Reader first: see "Behaviour to keep".
    let (bytes_tx, mut bytes_rx) = unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if bytes_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let id = spec.id;
    let exit_notes = notes.clone();
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|s| s.exit_code() as i32);
        let _ = exit_notes.send(SessionNote::Exited(id, code));
    });

    let (cmd_tx, mut cmd_rx): (UnboundedSender<SessionCmd>, UnboundedReceiver<SessionCmd>) =
        unbounded_channel();
    let mut term = TermCore::new(TermConfig::new(spec.cols, spec.rows));
    tokio::spawn(async move {
        let mut attached: HashMap<ClientId, (UnboundedSender<ServerEvent>, Option<Snapshot>)> =
            HashMap::new();
        let mut dirty = false;
        let mut last_activity_note = Instant::now() - Duration::from_secs(10);
        let mut tick = tokio::time::interval(Duration::from_millis(16));
        let mut output_open = true;
        loop {
            tokio::select! {
                bytes = bytes_rx.recv(), if output_open => match bytes {
                    Some(bytes) => {
                        for event in term.feed(&bytes) {
                            match event {
                                TermEvent::Reply(reply) => {
                                    let _ = writer.write_all(&reply);
                                    let _ = writer.flush();
                                }
                                TermEvent::Title(t) => { let _ = notes.send(SessionNote::Title(id, t)); }
                                TermEvent::Bell => {}
                            }
                        }
                        dirty = true;
                        if last_activity_note.elapsed() >= Duration::from_secs(1) {
                            last_activity_note = Instant::now();
                            let _ = notes.send(SessionNote::Activity(id));
                        }
                    }
                    None => output_open = false,
                },
                cmd = cmd_rx.recv() => match cmd {
                    None => {
                        let _ = killer.kill();
                        break;
                    }
                    Some(SessionCmd::Kill) => { let _ = killer.kill(); }
                    Some(SessionCmd::Input(data)) => {
                        let _ = writer.write_all(&data);
                        let _ = writer.flush();
                    }
                    Some(SessionCmd::Resize { cols, rows }) => {
                        if cols > 0 && rows > 0 && (cols, rows) != term.size() {
                            let _ = master.resize(pty_size(cols, rows));
                            term.resize(cols, rows);
                            dirty = true;
                        }
                    }
                    Some(SessionCmd::Attach { client, out }) => {
                        attached.insert(client, (out, None));
                        dirty = true;
                    }
                    Some(SessionCmd::Detach { client }) => { attached.remove(&client); }
                },
                _ = tick.tick() => {
                    if term.tick(Instant::now()) {
                        dirty = true;
                    }
                    if dirty && !attached.is_empty() {
                        let snap = term.snapshot();
                        attached.retain(|_, (out, last)| {
                            let alive = match diff(last.as_ref(), &snap) {
                                Some(update) => out.send(ServerEvent::Screen { session: id, update }).is_ok(),
                                None => true,
                            };
                            *last = Some(snap.clone());
                            alive
                        });
                    }
                    dirty = false;
                }
            }
        }
    });
    Ok(cmd_tx)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;
    use termist_core::Snapshot;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::time::timeout;

    fn spec(script: &str) -> SpawnSpec {
        SpawnSpec {
            id: SessionId::new(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: vec![],
            cols: 40,
            rows: 6,
        }
    }

    /// Attaches and applies updates until some line contains `needle`.
    async fn wait_for_text(cmd: &UnboundedSender<SessionCmd>, needle: &str) -> Snapshot {
        let (out, mut rx) = unbounded_channel();
        cmd.send(SessionCmd::Attach {
            client: ClientId(1),
            out,
        })
        .unwrap();
        let mut screen = Snapshot::default();
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerEvent::Screen { update, .. }) = rx.recv().await {
                    screen.apply(&update);
                    if (0..screen.rows as usize).any(|r| screen.line_text(r).contains(needle)) {
                        return screen.clone();
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "never saw {needle:?}; last screen: {:?}",
                (0..screen.rows as usize)
                    .map(|r| screen.line_text(r))
                    .collect::<Vec<_>>()
            )
        })
    }

    #[tokio::test]
    async fn output_reaches_an_attached_client() {
        let (notes, _n) = unbounded_channel();
        let cmd = spawn(spec("printf 'hello from pty'; sleep 5"), notes).unwrap();
        wait_for_text(&cmd, "hello from pty").await;
    }

    #[tokio::test]
    async fn input_is_written_to_the_pty() {
        let (notes, _n) = unbounded_channel();
        let cmd = spawn(spec("read line; echo \"got:$line\"; sleep 5"), notes).unwrap();
        cmd.send(SessionCmd::Input(b"ping\r".to_vec())).unwrap();
        wait_for_text(&cmd, "got:ping").await;
    }

    #[tokio::test]
    async fn terminal_queries_are_answered_through_the_pty() {
        // raw mode so the 6-byte CPR reply reaches `head` without a newline
        let script = "stty raw -echo; printf '\\033[6n'; head -c 6 >/dev/null; stty sane; echo answered; sleep 5";
        let (notes, _n) = unbounded_channel();
        let cmd = spawn(spec(script), notes).unwrap();
        wait_for_text(&cmd, "answered").await;
    }

    #[tokio::test]
    async fn exit_code_is_reported_and_the_last_screen_stays_attachable() {
        let (notes, mut notes_rx) = unbounded_channel();
        let s = spec("echo bye; exit 3");
        let id = s.id;
        let cmd = spawn(s, notes).unwrap();
        let exited = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(SessionNote::Exited(sid, code)) = notes_rx.recv().await {
                    return (sid, code);
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(exited, (id, Some(3)));
        wait_for_text(&cmd, "bye").await;
    }

    #[tokio::test]
    async fn title_changes_are_noted() {
        let (notes, mut notes_rx) = unbounded_channel();
        let cmd = spawn(spec("printf '\\033]0;Fix Login\\007'; sleep 5"), notes).unwrap();
        let title = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(SessionNote::Title(_, t)) = notes_rx.recv().await {
                    return t;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(title.as_deref(), Some("Fix Login"));
        cmd.send(SessionCmd::Kill).unwrap();
    }
}
