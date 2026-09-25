use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termist_core::Modes;

pub fn encode_key(key: &KeyEvent, modes: &Modes) -> Vec<u8> {
    let m = key.modifiers;
    let (shift, alt, ctrl) = (
        m.contains(KeyModifiers::SHIFT),
        m.contains(KeyModifiers::ALT),
        m.contains(KeyModifiers::CONTROL),
    );
    let param = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let mut out = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if alt {
                out.push(0x1b);
            }
            match ctrl_byte(c).filter(|_| ctrl) {
                Some(b) => out.push(b),
                None => out.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes()),
            }
        }
        KeyCode::Enter if shift || alt => out.extend_from_slice(b"\x1b\r"),
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => cursor_key(&mut out, b'A', param, modes),
        KeyCode::Down => cursor_key(&mut out, b'B', param, modes),
        KeyCode::Right => cursor_key(&mut out, b'C', param, modes),
        KeyCode::Left => cursor_key(&mut out, b'D', param, modes),
        KeyCode::Home => cursor_key(&mut out, b'H', param, modes),
        KeyCode::End => cursor_key(&mut out, b'F', param, modes),
        KeyCode::Insert => tilde_key(&mut out, 2, param),
        KeyCode::Delete => tilde_key(&mut out, 3, param),
        KeyCode::PageUp => tilde_key(&mut out, 5, param),
        KeyCode::PageDown => tilde_key(&mut out, 6, param),
        KeyCode::F(n @ 1..=4) => {
            let letter = *b"PQRS".get((n - 1) as usize).unwrap();
            if param > 1 {
                out.extend_from_slice(format!("\x1b[1;{param}{}", letter as char).as_bytes());
            } else {
                out.extend_from_slice(&[0x1b, b'O', letter]);
            }
        }
        KeyCode::F(n @ 5..=12) => tilde_key(
            &mut out,
            [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize],
            param,
        ),
        _ => {}
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(out: &mut Vec<u8>, letter: u8, param: u8, modes: &Modes) {
    if param > 1 {
        out.extend_from_slice(format!("\x1b[1;{param}{}", letter as char).as_bytes());
    } else if modes.app_cursor {
        out.extend_from_slice(&[0x1b, b'O', letter]);
    } else {
        out.extend_from_slice(&[0x1b, b'[', letter]);
    }
}

fn tilde_key(out: &mut Vec<u8>, code: u8, param: u8) {
    if param > 1 {
        out.extend_from_slice(format!("\x1b[{code};{param}~").as_bytes());
    } else {
        out.extend_from_slice(format!("\x1b[{code}~").as_bytes());
    }
}

pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    if modes.bracketed_paste {
        [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};

    fn key(code: K, mods: M) -> Vec<u8> {
        encode_key(&KeyEvent::new(code, mods), &Modes::default())
    }

    #[test]
    fn legacy_encoding_table() {
        let cases: Vec<(K, M, &[u8])> = vec![
            (K::Char('a'), M::NONE, b"a"),
            (K::Char('A'), M::SHIFT, b"A"),
            (K::Char('c'), M::CONTROL, b"\x03"),
            (K::Char(' '), M::CONTROL, b"\x00"),
            (K::Char('x'), M::ALT, b"\x1bx"),
            (K::Enter, M::NONE, b"\r"),
            (K::Enter, M::SHIFT, b"\x1b\r"),
            (K::Tab, M::NONE, b"\t"),
            (K::BackTab, M::SHIFT, b"\x1b[Z"),
            (K::Backspace, M::NONE, b"\x7f"),
            (K::Esc, M::NONE, b"\x1b"),
            (K::Up, M::NONE, b"\x1b[A"),
            (K::Left, M::CONTROL, b"\x1b[1;5D"),
            (K::Home, M::NONE, b"\x1b[H"),
            (K::PageDown, M::NONE, b"\x1b[6~"),
            (K::Delete, M::SHIFT, b"\x1b[3;2~"),
            (K::F(1), M::NONE, b"\x1bOP"),
            (K::F(5), M::NONE, b"\x1b[15~"),
            (K::F(12), M::NONE, b"\x1b[24~"),
        ];
        for (code, mods, want) in cases {
            assert_eq!(key(code, mods), want, "{code:?} {mods:?}");
        }
    }

    #[test]
    fn turkish_letters_are_utf8() {
        assert_eq!(key(K::Char('ş'), M::NONE), "ş".as_bytes());
        assert_eq!(key(K::Char('İ'), M::SHIFT), "İ".as_bytes());
    }

    #[test]
    fn application_cursor_mode_changes_arrows() {
        let modes = Modes {
            app_cursor: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_key(&KeyEvent::new(K::Up, M::NONE), &modes),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(&KeyEvent::new(K::End, M::NONE), &modes),
            b"\x1bOF"
        );
    }

    #[test]
    fn paste_is_bracketed_only_when_the_child_asked() {
        let on = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(encode_paste("a\nb", &on), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(encode_paste("a\nb", &Modes::default()), b"a\rb");
    }
}
