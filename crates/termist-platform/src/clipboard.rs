//! The system clipboard, for text copied out of the pane.
//!
//! Two ways at once: the OS clipboard, which works whatever tmux is set to, and an
//! OSC 52 sequence for the terminal, which reaches the clipboard over ssh.

/// The OS clipboard, kept open: on X11 the copied text lives only as long as it.
/// `default()` has none, for tests.
#[derive(Default)]
pub struct Clipboard(Option<arboard::Clipboard>);

impl Clipboard {
    /// Opens the OS clipboard; without one (no display, over ssh) only OSC 52 is left.
    pub fn open() -> Clipboard {
        Clipboard(arboard::Clipboard::new().ok())
    }

    /// Puts `text` on the OS clipboard; false when there is none or it refused.
    pub fn copy(&mut self, text: &str) -> bool {
        self.0
            .as_mut()
            .is_some_and(|c| c.set_text(text.to_owned()).is_ok())
    }
}

/// The OSC 52 sequence that asks the terminal to put `text` on its clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_carries_the_text_in_base64() {
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("abc"), "\x1b]52;c;YWJj\x07");
        assert_eq!(osc52(""), "\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_encodes_utf8_bytes() {
        // printf 'çağ\n' | base64
        assert_eq!(osc52("çağ\n"), "\x1b]52;c;w6dhxJ8K\x07");
    }
}
