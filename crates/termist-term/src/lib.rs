//! Terminal emulation for one session, wrapping alacritty_terminal.
mod convert;
mod scan;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, Rgb, StdSyncHandler};
use std::sync::mpsc;
use std::time::Instant;
use termist_core::{Cursor, Modes, Snapshot};

#[derive(Clone, Debug)]
pub struct TermConfig {
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    pub xtversion: String,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

impl TermConfig {
    pub fn new(cols: u16, rows: u16) -> TermConfig {
        TermConfig {
            cols,
            rows,
            scrollback: 10_000,
            xtversion: format!("termist {}", env!("CARGO_PKG_VERSION")),
            fg: (0xd8, 0xd8, 0xd8),
            bg: (0x1e, 0x1e, 0x1e),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// Bytes to write back to the PTY right away (answers to terminal queries).
    Reply(Vec<u8>),
    Title(Option<String>),
    Bell,
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
struct Collector(mpsc::Sender<Event>);

impl EventListener for Collector {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub struct TermCore {
    cfg: TermConfig,
    term: Term<Collector>,
    parser: Processor<StdSyncHandler>,
    events: mpsc::Receiver<Event>,
    xtversion: scan::XtVersionScanner,
}

impl TermCore {
    pub fn new(cfg: TermConfig) -> TermCore {
        let (tx, events) = mpsc::channel();
        let config = Config {
            scrolling_history: cfg.scrollback,
            kitty_keyboard: false,
            ..Config::default()
        };
        let size = Size {
            cols: cfg.cols.max(1) as usize,
            rows: cfg.rows.max(1) as usize,
        };
        let term = Term::new(config, &size, Collector(tx));
        TermCore {
            cfg,
            term,
            parser: Processor::new(),
            events,
            xtversion: scan::XtVersionScanner::default(),
        }
    }

    /// Feeds PTY output. Returned `Reply` events must be written to the PTY in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TermEvent> {
        let mut out = Vec::new();
        for _ in 0..self.xtversion.scan(bytes) {
            out.push(TermEvent::Reply(
                format!("\x1bP>|{}\x1b\\", self.cfg.xtversion).into_bytes(),
            ));
        }
        self.parser.advance(&mut self.term, bytes);
        self.drain(&mut out);
        out
    }

    /// When a pending DEC 2026 synchronized update times out, if one is pending.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Drives the DEC 2026 synchronized-update timeout. `Some` means the held-back
    /// output was applied (the screen may have changed); its events, such as replies
    /// to queries inside the update, are handled like those of `feed`.
    pub fn tick(&mut self, now: Instant) -> Option<Vec<TermEvent>> {
        if self.sync_deadline().is_some_and(|deadline| now >= deadline) {
            self.parser.stop_sync(&mut self.term);
            let mut out = Vec::new();
            self.drain(&mut out);
            return Some(out);
        }
        None
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.cfg.cols = cols;
        self.cfg.rows = rows;
        self.term.resize(Size {
            cols: cols as usize,
            rows: rows as usize,
        });
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cfg.cols, self.cfg.rows)
    }

    pub fn modes(&self) -> Modes {
        convert::modes(*self.term.mode())
    }

    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let (rows, cols) = (grid.screen_lines(), grid.columns());
        let lines = (0..rows)
            .map(|l| {
                let row = &grid[Line(l as i32)];
                (0..cols).map(|c| convert::cell(&row[Column(c)])).collect()
            })
            .collect();
        let p = grid.cursor.point;
        Snapshot {
            cols: cols as u16,
            rows: rows as u16,
            lines,
            cursor: Cursor {
                row: p.line.0.max(0) as u16,
                col: p.column.0 as u16,
            },
            modes: self.modes(),
        }
    }

    fn drain(&mut self, out: &mut Vec<TermEvent>) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::PtyWrite(s) => out.push(TermEvent::Reply(s.into_bytes())),
                Event::ColorRequest(index, format) => {
                    out.push(TermEvent::Reply(format(self.color(index)).into_bytes()))
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.cfg.rows,
                        num_cols: self.cfg.cols,
                        cell_width: 8,
                        cell_height: 16,
                    };
                    out.push(TermEvent::Reply(format(size).into_bytes()));
                }
                Event::Title(t) => out.push(TermEvent::Title(Some(t))),
                Event::ResetTitle => out.push(TermEvent::Title(None)),
                Event::Bell => out.push(TermEvent::Bell),
                _ => {}
            }
        }
    }

    fn color(&self, index: usize) -> Rgb {
        if let Some(c) = self.term.colors()[index] {
            return c;
        }
        let rgb = |(r, g, b): (u8, u8, u8)| Rgb { r, g, b };
        match index {
            256 | 258 => rgb(self.cfg.fg), // foreground, cursor
            257 => rgb(self.cfg.bg),       // background
            i => convert::xterm_rgb(i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::{Color, cell_flags};

    fn core() -> TermCore {
        TermCore::new(TermConfig::new(20, 5))
    }

    fn replies(events: &[TermEvent]) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|e| {
                if let TermEvent::Reply(b) = e {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn text_lands_on_screen_and_moves_the_cursor() {
        let mut t = core();
        t.feed(b"hello");
        let s = t.snapshot();
        assert_eq!(s.line_text(0), "hello");
        assert_eq!((s.cursor.row, s.cursor.col), (0, 5));
        assert_eq!((s.cols, s.rows), (20, 5));
    }

    #[test]
    fn sgr_colours_are_kept() {
        let mut t = core();
        t.feed(b"\x1b[31mR\x1b[38;2;1;2;3mG");
        let s = t.snapshot();
        assert_eq!(s.lines[0][0].fg, Color::Indexed(1));
        assert_eq!(s.lines[0][1].fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn device_attributes_and_cursor_position_are_answered() {
        let mut t = core();
        let r = replies(&t.feed(b"ab\x1b[6n\x1b[c"));
        assert!(r.contains(&b"\x1b[1;3R".to_vec()), "{r:?}");
        assert!(r.contains(&b"\x1b[?6c".to_vec()), "{r:?}");
    }

    #[test]
    fn a_query_inside_a_timed_out_synchronized_update_is_answered_by_tick() {
        let mut t = core();
        let now = Instant::now();
        assert!(
            replies(&t.feed(b"\x1b[?2026h\x1b[6n")).is_empty(),
            "held back"
        );
        let deadline = t.sync_deadline().expect("a synchronized update is pending");
        assert_eq!(t.tick(now), None, "not yet");
        let events = t.tick(deadline).expect("the update timed out");
        assert_eq!(replies(&events), vec![b"\x1b[1;1R".to_vec()]);
        assert_eq!(t.sync_deadline(), None);
    }

    #[test]
    fn xtversion_is_answered_even_across_chunks() {
        let mut t = core();
        let mut r = replies(&t.feed(b"\x1b[>"));
        r.extend(replies(&t.feed(b"q")));
        let expected = format!("\x1bP>|termist {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes();
        assert_eq!(r, vec![expected]);
    }

    #[test]
    fn background_colour_query_gets_the_host_background() {
        let mut t = core();
        let r = replies(&t.feed(b"\x1b]11;?\x07"));
        assert_eq!(r, vec![b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07".to_vec()]);
    }

    #[test]
    fn title_and_bell_are_reported() {
        let mut t = core();
        let ev = t.feed(b"\x1b]0;Fix Login\x07\x07");
        assert!(ev.contains(&TermEvent::Title(Some("Fix Login".into()))));
        assert!(ev.contains(&TermEvent::Bell));
    }

    #[test]
    fn modes_follow_the_child() {
        let mut t = core();
        assert!(!t.modes().alt_screen);
        t.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1h");
        let m = t.modes();
        assert!(m.alt_screen && m.bracketed_paste && m.app_cursor);
    }

    #[test]
    fn resize_changes_the_snapshot_shape() {
        let mut t = core();
        t.resize(30, 8);
        let s = t.snapshot();
        assert_eq!(
            (s.cols, s.rows, s.lines.len(), s.lines[0].len()),
            (30, 8, 8, 30)
        );
    }

    #[test]
    fn wide_characters_mark_their_spacer() {
        let mut t = core();
        t.feed("界".as_bytes());
        let s = t.snapshot();
        assert_ne!(s.lines[0][0].flags & cell_flags::WIDE, 0);
        assert_ne!(s.lines[0][1].flags & cell_flags::WIDE_SPACER, 0);
        assert_eq!(s.line_text(0), "界");
    }
}
