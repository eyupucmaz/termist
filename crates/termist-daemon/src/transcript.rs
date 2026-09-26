//! Claude fires no hook when a turn is cancelled (Esc) or a permission is denied; its
//! transcript gets an "[Request interrupted by user…]" line instead.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MARKER: &[u8] = b"[Request interrupted by user";
/// Never read more than this per poll (a huge paste in the transcript is not a marker).
const MAX_READ: u64 = 4 * 1024 * 1024;

pub struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl TranscriptTail {
    /// Starts at the file's current end: earlier history is never reported.
    pub fn new(path: PathBuf) -> TranscriptTail {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        TranscriptTail {
            path,
            offset,
            partial: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True when a complete line written since the last poll carries the marker.
    pub fn poll(&mut self) -> bool {
        let Ok(mut file) = File::open(&self.path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        if len < self.offset {
            // truncated or replaced: start over
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut buf = Vec::new();
        let Ok(n) = file
            .take((len - self.offset).min(MAX_READ))
            .read_to_end(&mut buf)
        else {
            return false;
        };
        self.offset += n as u64;
        self.partial.extend_from_slice(&buf);
        let mut found = false;
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            found |= line.windows(MARKER.len()).any(|w| w == MARKER);
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    const INTERRUPT: &str = r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;

    #[test]
    fn history_before_the_tail_started_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        append(&p, &format!("{INTERRUPT}\n"));
        let mut t = TranscriptTail::new(p.clone());
        assert!(!t.poll());
        append(&p, "{\"type\":\"assistant\"}\n");
        assert!(!t.poll());
        append(&p, &format!("{INTERRUPT}\n"));
        assert!(t.poll());
        assert!(!t.poll(), "each marker is reported once");
    }

    #[test]
    fn a_line_split_across_writes_is_seen_once_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        append(&p, "");
        let mut t = TranscriptTail::new(p.clone());
        let (a, b) = INTERRUPT.split_at(40);
        append(&p, a);
        assert!(!t.poll());
        append(&p, &format!("{b}\n"));
        assert!(t.poll());
    }

    #[test]
    fn missing_truncated_or_replaced_files_never_panic_or_misfire() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        let mut t = TranscriptTail::new(p.clone()); // does not exist yet
        assert!(!t.poll());
        append(&p, "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n");
        assert!(!t.poll());
        std::fs::write(&p, "").unwrap(); // truncated
        assert!(!t.poll());
        append(&p, &format!("{INTERRUPT}\n"));
        assert!(
            t.poll(),
            "after truncation the tail restarts from the beginning"
        );
    }
}
