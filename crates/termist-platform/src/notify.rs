//! Telling the user something happened: a sound through the platform's own player,
//! the terminal bell, and a desktop notification asked of the terminal.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The players tried in order, with the arguments before the file.
#[cfg(target_os = "macos")]
const PLAYERS: &[(&str, &[&str])] = &[("afplay", &[])];
#[cfg(all(unix, not(target_os = "macos")))]
const PLAYERS: &[(&str, &[&str])] = &[("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])];

/// Plays the sound file at `path` without waiting for it; false when no player could
/// be started (then the caller rings the bell).
pub fn play(path: &Path) -> bool {
    #[cfg(unix)]
    {
        for (program, args) in PLAYERS {
            if let Some(program) = find(program)
                && spawn(Command::new(program).args(*args).arg(path))
            {
                return true;
            }
        }
        false
    }
    #[cfg(windows)]
    {
        let script = format!(
            "(New-Object Media.SoundPlayer '{}').PlaySync()",
            path.display().to_string().replace('\'', "''")
        );
        spawn(
            Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command"])
                .arg(script),
        )
    }
}

/// The platform's own notification sound, if it has one we know.
pub fn system_sound() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/System/Library/Sounds/Glass.aiff"]
    } else if cfg!(windows) {
        &[r"C:\Windows\Media\Windows Notify System Generic.wav"]
    } else {
        &[
            "/usr/share/sounds/freedesktop/stereo/message.oga",
            "/usr/share/sounds/freedesktop/stereo/complete.oga",
        ]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Starts `cmd` detached from our terminal, reaping it on a thread.
fn spawn(cmd: &mut Command) -> bool {
    match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn find(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// How a terminal takes a desktop notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// OSC 777 (Ghostty, WezTerm, foot, rxvt): a title and a body.
    Osc777,
    /// OSC 99 (kitty).
    Osc99,
    /// OSC 9 (iTerm2, Windows Terminal and many others): one line.
    Osc9,
}

/// The dialect of the terminal `var` describes (TERM_PROGRAM, TERM), and whether it
/// is behind tmux, which must pass the sequence through.
pub fn dialect(var: impl Fn(&str) -> Option<String>) -> (Dialect, bool) {
    let tmux = var("TMUX").is_some_and(|v| !v.is_empty());
    let program = var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    let term = var("TERM").unwrap_or_default();
    let dialect = if program == "ghostty" || program == "wezterm" || term.starts_with("foot") {
        Dialect::Osc777
    } else if term == "xterm-kitty" {
        Dialect::Osc99
    } else {
        Dialect::Osc9
    };
    (dialect, tmux)
}

/// The escape sequence that asks the terminal for a desktop notification.
pub fn desktop_notification(dialect: Dialect, tmux: bool, title: &str, body: &str) -> String {
    // Text only: no control characters, no field separators.
    let clean = |s: &str| {
        s.chars()
            .filter(|c| !c.is_control() && *c != ';')
            .collect::<String>()
    };
    let (title, body) = (clean(title), clean(body));
    let seq = match dialect {
        Dialect::Osc777 => format!("\x1b]777;notify;{title};{body}\x1b\\"),
        Dialect::Osc99 => {
            format!("\x1b]99;i=1:d=0;{title}\x1b\\\x1b]99;i=1:d=1:p=body;{body}\x1b\\")
        }
        Dialect::Osc9 => format!("\x1b]9;{title}: {body}\x07"),
    };
    if tmux {
        // tmux passes a DCS through to the outer terminal, every ESC doubled.
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn each_terminal_gets_its_own_sequence() {
        assert_eq!(
            dialect(env(&[("TERM_PROGRAM", "ghostty")])),
            (Dialect::Osc777, false)
        );
        assert_eq!(
            dialect(env(&[("TERM", "xterm-kitty")])),
            (Dialect::Osc99, false)
        );
        assert_eq!(
            dialect(env(&[("TERM_PROGRAM", "iTerm.app")])),
            (Dialect::Osc9, false)
        );
        assert_eq!(
            dialect(env(&[("TERM_PROGRAM", "tmux"), ("TMUX", "/tmp/t,1,0")])),
            (Dialect::Osc9, true)
        );
    }

    #[test]
    fn notifications_are_well_formed_and_clean() {
        assert_eq!(
            desktop_notification(Dialect::Osc777, false, "termist", "claude-1; waits\x07"),
            "\x1b]777;notify;termist;claude-1 waits\x1b\\"
        );
        assert_eq!(
            desktop_notification(Dialect::Osc9, false, "termist", "done"),
            "\x1b]9;termist: done\x07"
        );
        assert_eq!(
            desktop_notification(Dialect::Osc9, true, "t", "b"),
            "\x1bPtmux;\x1b\x1b]9;t: b\x07\x1b\\"
        );
    }

    #[test]
    fn a_missing_file_or_player_is_not_a_crash() {
        let _ = play(Path::new("/nonexistent/termist.wav"));
        if let Some(sound) = system_sound() {
            assert!(sound.is_file());
        }
    }
}
