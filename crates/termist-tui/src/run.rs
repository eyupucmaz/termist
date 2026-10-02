use crate::app::{Action, App};
use crate::browse::{self, Listing};
use crate::keys::Keymap;
use crate::settings;
use crate::sound::Recording;
use crate::theme::{Theme, Themes};
use crate::ui;
use anyhow::{Context, bail};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::Rect;
use std::io::Write;
use std::io::stdout;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use termist_core::config::{ColorDepth, Problem, Sound, StatusConfig};
use termist_core::{ClientRequest, ServerEvent, TermColors};
use termist_platform::clipboard::{self, Clipboard};
use termist_platform::config_file;
use termist_platform::framed::write_frame;
use termist_platform::host_colors;
use termist_platform::ipc::SendHalf;
use termist_platform::notify;
use termist_platform::sysstat::{Sampler, SysStat};
use termist_platform::{Client, Paths};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

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

/// The app with the user's settings; what could not be used is said once in the footer.
fn app_from_config(paths: &Paths, inside_tmux: bool) -> App {
    let (config, mut problems) = termist_platform::config_file::load(paths);
    let detected = termist_platform::term::resolve_depth(ColorDepth::Auto);
    let depth = match config.colors {
        ColorDepth::Auto => detected,
        depth => depth,
    };
    let (themes, theme_problems) = Themes::load(&paths.themes_dir());
    problems.extend(theme_problems);
    problems.extend(themes.check(&config.theme));
    let theme = themes.get(&config.theme, depth);
    let (keymap, key_problems) = Keymap::from_config(&config.keys, &config.prefix);
    problems.extend(key_problems);
    let mut app = App::with_config(config, theme, keymap);
    app.themes = themes;
    app.config_path = Some(paths.config_path());
    app.detected_depth = detected;
    app.local_settings = std::fs::read_to_string(paths.config_local_path())
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .map(|t| local_settings(&t))
        .unwrap_or_default();
    app.message = startup_message(&problems, &app.theme);
    if app.message.is_none() {
        app.message = tmux_notice(paths, &app.keymap, inside_tmux);
    }
    app
}

/// The settings a config.local.toml sets: `theme`, `notify.sounds` (tables merge key by
/// key), and `keys` for any key binding.
fn local_settings(table: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in table {
        match value.as_table() {
            Some(inner) if key != "keys" => {
                out.extend(inner.keys().map(|k| format!("{key}.{k}")));
            }
            _ => out.push(key.clone()),
        }
    }
    out
}

/// Inside tmux, C-a is usually tmux's own prefix too: said once, then remembered.
fn tmux_notice(paths: &Paths, keymap: &Keymap, inside_tmux: bool) -> Option<String> {
    if !inside_tmux || keymap.prefix.to_string() != "C-a" {
        return None;
    }
    let marker = paths.notices_dir().join("tmux-prefix");
    if marker.exists() {
        return None;
    }
    let _ = std::fs::create_dir_all(paths.notices_dir());
    let _ = std::fs::write(&marker, "");
    Some(TMUX_NOTICE.into())
}

const TMUX_NOTICE: &str =
    "Inside tmux, C-a is tmux's prefix too · s → prefix changes it (C-Space is free)";

fn startup_message(problems: &[Problem], theme: &Theme) -> Option<String> {
    match problems {
        [] => theme.stands_in_for.as_ref().map(|wanted| {
            format!("{wanted} needs 256 colours; showing the terminal's own (colors = \"256\" if it has them)")
        }),
        [one] => Some(format!("config: {one} · termist config check")),
        many => Some(format!(
            "config: {} problems · termist config check",
            many.len()
        )),
    }
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
    let inside_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let mut app = app_from_config(&paths, inside_tmux);

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
    // A theme that paints nothing shows agents in the host terminal's own colours;
    // they are asked for once, here, in case the settings switch to such a theme.
    // Every terminal answers the query's last part at once, which ends the wait; the
    // long limit is for slow links (ssh), whose late answers would otherwise arrive
    // as keys.
    app.host_colors = host_colors::query(Duration::from_secs(1)).map(|(fg, bg)| TermColors {
        fg,
        bg,
        ansi: None,
    });
    write_frame(&mut writer, &ClientRequest::SetColors(app.agent_colors())).await?;
    write_frame(
        &mut writer,
        &ClientRequest::SetGitHub {
            enabled: app.config.github.enabled,
        },
    )
    .await?;
    let _ = execute!(stdout(), EnableBracketedPaste, EnableFocusChange);
    let mut mouse_on = set_mouse(false, app.config.mouse);
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
            if !wanted(&ev) {
                continue;
            }
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let (listing_tx, mut listing_rx) = unbounded_channel::<Listed>();
    let (bell_tx, mut bell_rx) = unbounded_channel::<()>();
    let mut alerts = Alerts::from_env(bell_tx);
    let mut clipboard = Clipboard::open();
    let (stat_tx, mut stat_rx) = unbounded_channel::<SysStat>();
    let status_wanted = Arc::new(AtomicU8::new(status_mask(&app.config.status)));
    let focused = Arc::new(AtomicBool::new(true));
    let mut sampler_started = false;
    app.start_splash(Instant::now());
    let result: anyhow::Result<()> = async {
        loop {
            // Here, not only on a timer: steady output (a busy agent's screen) would keep
            // any timer from ever firing, and the splash from ever ending.
            let now = Instant::now();
            (app.hour, app.minute) = termist_platform::clock::local_time();
            let mask = status_mask(&app.config.status);
            status_wanted.store(mask, Ordering::Relaxed);
            focused.store(app.window_focused, Ordering::Relaxed);
            if mask != 0 && !sampler_started {
                spawn_sampler(status_wanted.clone(), focused.clone(), stat_tx.clone());
                sampler_started = true;
            }
            app.tick(now);
            let size = terminal.size()?;
            let areas = ui::layout(
                Rect::new(0, 0, size.width, size.height),
                app.project_sessions().len(),
                app.pane_position(),
            );
            app.pane_right = areas.pane_right;
            app.pane_area = areas.pane_inner;
            app.screen = Rect::new(0, 0, size.width, size.height);
            app.set_card_window(areas.cards_per_row, areas.card_rows);
            let resize = app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
            if perform(resize, &mut writer, &listing_tx).await? {
                return Ok(());
            }
            terminal.draw(|f| ui::draw(f, &app, &areas))?;
            let wake = app.next_wake(Instant::now());
            let actions = tokio::select! {
                ev = input_rx.recv() => match ev {
                    Some(Event::Key(k)) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => app.on_key(k),
                    Some(Event::Paste(text)) => app.on_paste(&text),
                    Some(Event::Mouse(m)) => app.on_mouse(m),
                    Some(Event::FocusGained) => { app.window_focused = true; vec![] }
                    Some(Event::FocusLost) => { app.window_focused = false; vec![] }
                    Some(_) => vec![],
                    None => return Ok(()),
                },
                ev = server_rx.recv() => match ev {
                    Some(ev) => app.on_event(ev),
                    None => bail!("the termist daemon went away"),
                },
                Some((dir, listing)) = listing_rx.recv() => {
                    app.listed(&dir, listing);
                    vec![]
                }
                // The next frame of a scene, the end of the splash, the idle screen.
                _ = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now).into()), if wake.is_some() => vec![],
                Some(()) = bell_rx.recv() => {
                    ring_bell();
                    vec![]
                }
                Some(stat) = stat_rx.recv() => {
                    app.sysstat = stat;
                    vec![]
                }
            };
            let actions = save_settings(&paths, &mut app, actions);
            let actions = copy_out(&mut clipboard, &mut stdout(), actions);
            mouse_on = set_mouse(mouse_on, app.config.mouse);
            let actions = alerts.give(&paths, &app, actions);
            if perform(actions, &mut writer, &listing_tx).await? {
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
    let _ = execute!(
        stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture
    );
}

/// Takes the mouse from the terminal, or gives it back, when `wanted` changes.
fn set_mouse(on: bool, wanted: bool) -> bool {
    if on != wanted {
        let _ = if wanted {
            execute!(stdout(), EnableMouseCapture)
        } else {
            execute!(stdout(), DisableMouseCapture)
        };
    }
    wanted
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

const CPU: u8 = 1;
const RAM: u8 = 2;
const BATTERY: u8 = 4;
const CLOCK: u8 = 8;
/// The battery changes slowly.
const BATTERY_EVERY: Duration = Duration::from_secs(30);

fn status_mask(cfg: &StatusConfig) -> u8 {
    [
        (cfg.cpu, CPU),
        (cfg.ram, RAM),
        (cfg.battery, BATTERY),
        (cfg.clock, CLOCK),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(0, |m, (_, bit)| m | bit)
}

/// How often the status line is read: less when nobody is looking.
fn interval(focused: bool) -> Duration {
    Duration::from_secs(if focused { 2 } else { 10 })
}

/// Reads the machine on a thread of its own and sends each reading; every reading also
/// wakes the loop, which moves the clock on. With every part off it only sleeps.
fn spawn_sampler(mask: Arc<AtomicU8>, focused: Arc<AtomicBool>, out: UnboundedSender<SysStat>) {
    std::thread::spawn(move || {
        let mut sampler = Sampler::new();
        let mut battery_read: Option<Instant> = None;
        let mut last = SysStat::default();
        loop {
            std::thread::sleep(interval(focused.load(Ordering::Relaxed)));
            let mask = mask.load(Ordering::Relaxed);
            if mask == 0 {
                continue;
            }
            let (cpu, ram) = if mask & (CPU | RAM) != 0 {
                sampler.cpu_ram()
            } else {
                (None, None)
            };
            last.cpu = cpu.filter(|_| mask & CPU != 0);
            last.ram = ram.filter(|_| mask & RAM != 0);
            if mask & BATTERY == 0 {
                last.battery = None;
            } else if battery_read.is_none_or(|t| t.elapsed() >= BATTERY_EVERY) {
                last.battery = sampler.battery();
                battery_read = Some(Instant::now());
            }
            if out.send(last).is_err() {
                return;
            }
        }
    });
}

/// Puts the text copied among `actions` on the clipboard and returns the rest: on the
/// OS clipboard, and through OSC 52 on the terminal's, which is the one that reaches
/// the user over ssh. tmux passes OSC 52 on only with `set-clipboard on`.
fn copy_out(clipboard: &mut Clipboard, out: &mut impl Write, actions: Vec<Action>) -> Vec<Action> {
    let (copies, rest): (Vec<_>, Vec<_>) = actions
        .into_iter()
        .partition(|a| matches!(a, Action::Copy(_)));
    for copy in copies {
        let Action::Copy(text) = copy else {
            continue;
        };
        clipboard.copy(&text);
        let _ = out.write_all(clipboard::osc52(&text).as_bytes());
        let _ = out.flush();
    }
    rest
}

/// Mouse capture reports every move; only buttons, left drags and the wheel are used.
fn wanted(ev: &Event) -> bool {
    !matches!(
        ev,
        Event::Mouse(m) if matches!(m.kind, MouseEventKind::Moved)
            || matches!(m.kind, MouseEventKind::Drag(b) if b != MouseButton::Left)
    )
}

/// Writes the settings changes among `actions` to config.toml and returns the rest.
/// A change that cannot be saved still holds until termist quits, and says so.
fn save_settings(paths: &Paths, app: &mut App, actions: Vec<Action>) -> Vec<Action> {
    let (edits, rest): (Vec<_>, Vec<_>) = actions
        .into_iter()
        .partition(|a| matches!(a, Action::WriteConfig(_)));
    for edit in edits {
        let Action::WriteConfig(edit) = edit else {
            continue;
        };
        let text = match std::fs::read_to_string(paths.config_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            // Never replace a file that could not be read.
            Err(e) => {
                app.message = Some(format!("not saved: config.toml cannot be read ({e})"));
                continue;
            }
        };
        let saved = settings::apply(&text, &edit)
            .and_then(|new| config_file::write(paths, &new).map_err(|e| e.to_string()));
        if let Err(e) = saved {
            app.message = Some(format!("not saved: {e}"));
        }
    }
    rest
}

/// Sounds and desktop notifications for agents that start waiting or finish.
struct Alerts {
    dialect: (notify::Dialect, bool),
    last_sound: Option<Instant>,
    /// A player that started but could not play asks for the bell here.
    bell: UnboundedSender<()>,
}

/// Two sounds within this long are one: several cards finishing together.
const SOUND_GAP: Duration = Duration::from_secs(1);

impl Alerts {
    fn from_env(bell: UnboundedSender<()>) -> Alerts {
        Alerts {
            dialect: notify::dialect(|k| std::env::var(k).ok()),
            last_sound: None,
            bell,
        }
    }

    /// Gives the alerts and sound previews among `actions` and returns the rest.
    fn give(&mut self, paths: &Paths, app: &App, actions: Vec<Action>) -> Vec<Action> {
        let (alerts, rest): (Vec<_>, Vec<_>) = actions
            .into_iter()
            .partition(|a| matches!(a, Action::Alert(_) | Action::Preview(_)));
        for alert in alerts {
            let alert = match alert {
                Action::Alert(alert) => alert,
                Action::Preview(chosen) => {
                    let bell = self.bell.clone();
                    sound(paths, chosen, move || {
                        let _ = bell.send(());
                    });
                    continue;
                }
                _ => continue,
            };
            let now = Instant::now();
            if self
                .last_sound
                .is_none_or(|t| now.duration_since(t) >= SOUND_GAP)
            {
                self.last_sound = Some(now);
                let bell = self.bell.clone();
                let chosen = if alert.waiting {
                    app.config.notify.waiting_sound
                } else {
                    app.config.notify.done_sound
                };
                sound(paths, chosen, move || {
                    let _ = bell.send(());
                });
            }
            if app.config.notify.desktop && !app.window_focused {
                let (dialect, tmux) = self.dialect;
                let seq = notify::desktop_notification(dialect, tmux, "termist", &alert.text);
                let _ = stdout()
                    .write_all(seq.as_bytes())
                    .and_then(|()| stdout().flush());
            }
        }
        rest
    }
}

/// Plays a sound as the settings say: termist's martı or kedi, the system's, or the
/// bell. `failed` asks for the bell later, if a player starts but cannot play.
fn sound(paths: &Paths, setting: Sound, failed: impl FnOnce() + Send + 'static) {
    let file = match setting {
        Sound::Off => return,
        Sound::Bell => None,
        Sound::Marti => recording(paths, Recording::Marti),
        Sound::Kedi => recording(paths, Recording::Kedi),
        Sound::System => notify::system_sound(),
    };
    let played = file.is_some_and(|file| notify::play(&file, failed));
    if !played {
        ring_bell();
    }
}

fn recording(paths: &Paths, recording: Recording) -> Option<PathBuf> {
    crate::sound::file(&paths.data_dir.join("sounds"), recording).ok()
}

fn ring_bell() {
    let _ = stdout().write_all(b"\x07").and_then(|()| stdout().flush());
}

/// A folder listing, for the folder it lists.
type Listed = (PathBuf, Result<Listing, String>);

/// Sends requests and starts folder listings; returns `true` when the user asked to quit.
async fn perform(
    actions: Vec<Action>,
    writer: &mut SendHalf,
    listings: &UnboundedSender<Listed>,
) -> anyhow::Result<bool> {
    for action in actions {
        match action {
            Action::Send(req) => write_frame(writer, &req).await?,
            // A slow or hung folder (a network mount) blocks only that thread. It is a
            // detached thread, not a runtime task, so quitting never waits for it.
            Action::ListDir(dir) => {
                let listings = listings.clone();
                std::thread::spawn(move || {
                    let listing = browse::list_dir(&dir);
                    let _ = listings.send((dir, listing));
                });
            }
            Action::Quit => return Ok(true),
            // done by `save_settings`, `Alerts`, `copy_out`
            Action::WriteConfig(_) | Action::Alert(_) | Action::Preview(_) | Action::Copy(_) => {}
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
    fn the_sampler_reads_only_what_is_on() {
        let cfg = StatusConfig {
            cpu: true,
            ram: false,
            battery: true,
            clock: false,
        };
        assert_eq!(status_mask(&cfg), CPU | BATTERY);
        assert_eq!(
            status_mask(&StatusConfig::default()),
            CPU | RAM | BATTERY | CLOCK
        );
    }

    #[test]
    fn a_window_in_the_background_is_read_less_often() {
        assert_eq!(interval(true), Duration::from_secs(2));
        assert_eq!(interval(false), Duration::from_secs(10));
    }

    #[test]
    fn the_input_thread_keeps_left_drags_and_drops_moves() {
        use ratatui::crossterm::event::{KeyModifiers, MouseEvent};
        let mouse = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(!wanted(&mouse(MouseEventKind::Moved)));
        assert!(!wanted(&mouse(MouseEventKind::Drag(MouseButton::Right))));
        assert!(wanted(&mouse(MouseEventKind::Drag(MouseButton::Left))));
        assert!(wanted(&mouse(MouseEventKind::Down(MouseButton::Left))));
        assert!(wanted(&Event::FocusGained));
    }

    #[test]
    fn a_copy_goes_to_the_terminal_as_osc52() {
        let mut out = Vec::new();
        let rest = copy_out(
            &mut Clipboard::default(),
            &mut out,
            vec![Action::Copy("hi".into()), Action::Quit],
        );
        assert_eq!(rest, vec![Action::Quit]);
        assert_eq!(out, b"\x1b]52;c;aGk=\x07");
    }

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

    #[test]
    fn the_config_is_used_and_its_problems_are_said_once() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.config_path(),
            "theme = \"moda\"\ncolors = \"truecolor\"\n[agents]\ndefault = \"codex\"\n",
        )
        .unwrap();
        let app = app_from_config(&paths, false);
        assert_eq!(app.theme.id, "moda");
        assert_eq!(app.config.agents.default, termist_core::Harness::Codex);
        assert_eq!(app.message, None);

        std::fs::write(paths.config_path(), "theme = \"nope\"\ncolors = \"256\"\n").unwrap();
        let app = app_from_config(&paths, false);
        assert_eq!(app.theme.id, "uskudar", "the default theme");
        assert_eq!(
            app.message.as_deref(),
            Some(
                "config: theme: unknown theme \"nope\"; themes: uskudar, moda, aksaray, kadikoy, besiktas, balat, kapalicarsi, adalar, bebek, catppuccin-mocha, catppuccin-latte, tokyo-night, gruvbox-dark, nord, dracula, terminal · termist config check"
            )
        );
    }

    #[test]
    fn a_theme_the_terminal_cannot_draw_is_explained() {
        let theme = Theme::named("moda", termist_core::config::ColorDepth::Ansi16);
        let message = startup_message(&[], &theme).unwrap();
        assert!(message.starts_with("moda needs 256 colours"), "{message}");
        let two = [
            Problem {
                path: "a".into(),
                message: "x".into(),
            },
            Problem {
                path: "b".into(),
                message: "y".into(),
            },
        ];
        assert_eq!(
            startup_message(&two, &Theme::terminal()).as_deref(),
            Some("config: 2 problems · termist config check")
        );
    }

    #[test]
    fn inside_tmux_the_prefix_notice_is_said_once() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        let first = tmux_notice(&paths, &Keymap::defaults(), true);
        let second = tmux_notice(&paths, &Keymap::defaults(), true);
        let (other, _) = Keymap::from_config(&Default::default(), "C-Space");
        let tmp2 = tempfile::tempdir().unwrap();
        let other_paths = Paths::under(tmp2.path().to_path_buf());
        let with_other_prefix = tmux_notice(&other_paths, &other, true);
        let outside = tmux_notice(&other_paths, &Keymap::defaults(), false);
        assert_eq!(first.as_deref(), Some(TMUX_NOTICE));
        assert_eq!(second, None, "once");
        assert_eq!(with_other_prefix, None);
        assert_eq!(outside, None);
    }

    #[test]
    fn settings_are_saved_into_the_users_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_path(), "# mine\ntheme = \"uskudar\" # dark\n").unwrap();
        let mut app = App::new();
        let edit = |key, value: &str| {
            Action::WriteConfig(settings::ConfigEdit::Set {
                key,
                value: value.into(),
            })
        };
        let rest = save_settings(&paths, &mut app, vec![edit("theme", "moda"), Action::Quit]);
        assert_eq!(rest, vec![Action::Quit]);
        let text = std::fs::read_to_string(paths.config_path()).unwrap();
        assert_eq!(text, "# mine\ntheme = \"moda\" # dark\n");
        assert_eq!(app.message, None);

        std::fs::write(paths.config_path(), "theme = \"moda\n").unwrap();
        save_settings(&paths, &mut app, vec![edit("theme", "uskudar")]);
        assert!(
            app.message
                .unwrap()
                .starts_with("not saved: config.toml is not valid TOML")
        );
    }

    #[test]
    fn a_config_that_cannot_be_read_is_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let latin1 = b"# caf\xe9\ntheme = \"moda\"\n";
        std::fs::write(paths.config_path(), latin1).unwrap();
        let mut app = App::new();
        let edit = Action::WriteConfig(settings::ConfigEdit::Set {
            key: "theme",
            value: "uskudar".into(),
        });
        save_settings(&paths, &mut app, vec![edit]);
        assert!(
            app.message
                .unwrap()
                .starts_with("not saved: config.toml cannot be read")
        );
        assert_eq!(std::fs::read(paths.config_path()).unwrap(), latin1);
    }

    #[test]
    fn the_local_file_sets_keys_of_tables_not_whole_tables() {
        let t: toml::Table =
            "theme = \"moda\"\n[scenes]\npool = [\"galata\"]\n[keys.grid]\ng = \"help\"\n"
                .parse()
                .unwrap();
        assert_eq!(local_settings(&t), ["keys", "scenes.pool", "theme"]);
    }
}
