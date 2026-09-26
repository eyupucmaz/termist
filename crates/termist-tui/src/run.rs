use crate::app::{Action, App};
use crate::ui;
use anyhow::{Context, bail};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::Rect;
use std::io::stdout;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use termist_core::{ClientRequest, ServerEvent};
use termist_platform::framed::write_frame;
use termist_platform::ipc::SendHalf;
use termist_platform::{Client, Paths};
use tokio::sync::mpsc::unbounded_channel;

pub async fn connect_or_spawn(paths: &Paths) -> anyhow::Result<Client> {
    match Client::connect(paths).await {
        Ok(client) => return Ok(client),
        // A live daemon that speaks another protocol: spawning one more can't help.
        Err(e) if format!("{e:#}").contains("refused the connection") => return Err(e),
        Err(_) => {}
    }
    spawn_daemon(paths)?;
    for _ in 0..250 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if let Ok(client) = Client::connect(paths).await {
            return Ok(client);
        }
    }
    bail!(
        "could not start the termist daemon; see {}",
        paths.daemon_log_path().display()
    )
}

fn spawn_daemon(paths: &Paths) -> anyhow::Result<()> {
    paths.ensure()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.daemon_log_path())?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("daemon")
        .current_dir(daemon_cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; it detaches the daemon from our terminal
        // so it survives the TUI and never receives its Ctrl+C.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()?;
    Ok(())
}

/// The autostarted daemon outlives this TUI, so it must not keep the TUI's cwd (a
/// project or worktree the user may delete or unmount): it runs from the home dir.
fn daemon_cwd() -> PathBuf {
    std::env::home_dir()
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| {
            #[cfg(unix)]
            {
                PathBuf::from("/")
            }
            #[cfg(windows)]
            {
                std::env::temp_dir()
            }
        })
}

pub async fn run(paths: Paths) -> anyhow::Result<()> {
    let client = connect_or_spawn(&paths).await?;
    let (mut reader, mut writer) = client.into_split();
    write_frame(
        &mut writer,
        &ClientRequest::AddProject {
            path: std::env::current_dir()?,
        },
    )
    .await?;
    write_frame(&mut writer, &ClientRequest::ListState).await?;

    let (server_tx, mut server_rx) = unbounded_channel::<ServerEvent>();
    tokio::spawn(async move {
        while let Ok(Some(event)) = reader.read::<ServerEvent>().await {
            if server_tx.send(event).is_err() {
                break;
            }
        }
    });
    let mut terminal = ratatui::try_init().context("termist needs an interactive terminal")?;
    // Query before the input thread exists: `event::read()` holds crossterm's global
    // event-reader lock, and a query that can't take it times out after 2 s.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    let _ = execute!(stdout(), EnableBracketedPaste);
    if enhanced {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    set_panic_hook(enhanced);

    let (input_tx, mut input_rx) = unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut app = App::new();
    let result: anyhow::Result<()> = async {
        loop {
            let size = terminal.size()?;
            let areas = ui::layout(Rect::new(0, 0, size.width, size.height), app.project_sessions().len());
            app.cards_per_row = areas.cards_per_row;
            let resize = app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
            if perform(resize, &mut writer).await? {
                return Ok(());
            }
            terminal.draw(|f| ui::draw(f, &app, &areas))?;
            let actions = tokio::select! {
                ev = input_rx.recv() => match ev {
                    Some(Event::Key(k)) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => app.on_key(k),
                    Some(Event::Paste(text)) => app.on_paste(&text),
                    Some(_) => vec![],
                    None => return Ok(()),
                },
                ev = server_rx.recv() => match ev {
                    Some(ev) => app.on_event(ev),
                    None => bail!("the termist daemon went away"),
                },
            };
            if perform(actions, &mut writer).await? {
                return Ok(());
            }
        }
    }
    .await;

    undo_terminal_modes(enhanced);
    ratatui::restore();
    result
}

/// Undoes what `run` turns on beyond ratatui's raw mode and alternate screen.
fn undo_terminal_modes(enhanced: bool) {
    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout(), DisableBracketedPaste);
}

/// On a panic, pops the keyboard flags and disables bracketed paste, then runs the
/// previous hook: ratatui's (installed by `try_init`) restores raw mode and the
/// alternate screen, and then the default hook prints the panic.
fn set_panic_hook(enhanced: bool) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        undo_terminal_modes(enhanced);
        previous(info);
    }));
}

/// Sends requests; returns `true` when the user asked to quit.
async fn perform(actions: Vec<Action>, writer: &mut SendHalf) -> anyhow::Result<bool> {
    for action in actions {
        match action {
            Action::Send(req) => write_frame(writer, &req).await?,
            Action::Quit => return Ok(true),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_platform::framed::FramedReader;
    use termist_platform::ipc;

    #[test]
    fn the_daemon_runs_from_an_existing_dir_outside_the_project() {
        let dir = daemon_cwd();
        assert!(dir.is_dir(), "{dir:?}");
        if let Some(home) = std::env::home_dir().filter(|d| d.is_dir()) {
            assert_eq!(dir, home);
        }
    }

    // A daemon that speaks another protocol refuses the handshake; starting a second
    // daemon can't help (it would lose the lock race), so that error must surface as is.
    #[tokio::test]
    async fn a_protocol_mismatch_is_reported_instead_of_spawning_another() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        paths.ensure().unwrap();
        let listener = ipc::listen(&paths).unwrap();
        tokio::spawn(async move {
            use interprocess::local_socket::tokio::prelude::*;
            while let Ok(conn) = listener.accept().await {
                let (r, mut w) = conn.split();
                let _ = FramedReader::new(r).read::<ClientRequest>().await;
                let message = "protocol 1 is not supported (daemon speaks 99)".to_string();
                let _ = write_frame(&mut w, &ServerEvent::Error { message }).await;
            }
        });
        let err = connect_or_spawn(&paths)
            .await
            .err()
            .expect("a refused handshake must be an error");
        assert!(
            format!("{err:#}").contains("refused the connection"),
            "{err:#}"
        );
        assert!(
            !paths.daemon_log_path().exists(),
            "no daemon may be spawned for a protocol mismatch"
        );
    }
}
