/// Finds XTVERSION queries (`CSI > q`, `CSI > 0 q`) in the byte stream. alacritty_terminal
/// ignores them, but Claude Code only turns on synchronized output once it gets an answer.
#[derive(Default)]
pub struct XtVersionScanner {
    tail: Vec<u8>,
}

const PATTERNS: [&[u8]; 2] = [b"\x1b[>q", b"\x1b[>0q"];
const KEEP: usize = 4; // longest pattern minus one

impl XtVersionScanner {
    pub fn scan(&mut self, bytes: &[u8]) -> usize {
        let mut hay = std::mem::take(&mut self.tail);
        let carried = hay.len();
        hay.extend_from_slice(bytes);
        let mut found = 0;
        for pat in PATTERNS {
            for end in pat.len()..=hay.len() {
                // only count matches that finish in the new bytes
                if end > carried && &hay[end - pat.len()..end] == pat {
                    found += 1;
                }
            }
        }
        let keep = hay.len().min(KEEP);
        self.tail = hay[hay.len() - keep..].to_vec();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::XtVersionScanner;

    #[test]
    fn counts_each_query_once_whether_split_or_not() {
        let mut s = XtVersionScanner::default();
        assert_eq!(s.scan(b"x\x1b[>qy\x1b[>0q"), 2);
        assert_eq!(s.scan(b"\x1b["), 0);
        assert_eq!(s.scan(b">q"), 1);
        assert_eq!(
            s.scan(b"q"),
            0,
            "a lone q after a finished query must not recount"
        );
    }
}
