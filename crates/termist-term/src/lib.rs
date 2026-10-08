//! Terminal emulation for one session, wrapping alacritty_terminal.
mod convert;
mod scan;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll as GridScroll};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, Rgb, StdSyncHandler};
use std::sync::mpsc;
use std::time::Instant;
use termist_core::{Cursor, Modes, Pos, Scroll, ScrollPos, Snapshot, TermColors};

#[derive(Clone, Debug)]
pub struct TermConfig {
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    pub xtversion: String,
    /// What colour queries are answered with, unless the child set a colour itself.
    pub colors: TermColors,
}

impl TermConfig {
    pub fn new(cols: u16, rows: u16) -> TermConfig {
        TermConfig {
            cols,
            rows,
            scrollback: 10_000,
            xtversion: format!("termist {}", env!("CARGO_PKG_VERSION")),
            colors: TermColors::default(),
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

    /// The colours later queries are answered with.
    pub fn set_colors(&mut self, colors: TermColors) {
        self.cfg.colors = colors;
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cfg.cols, self.cfg.rows)
    }

    pub fn modes(&self) -> Modes {
        convert::modes(*self.term.mode())
    }

    /// Moves the view through the history. Output that comes while the view is back
    /// there leaves it on the same lines.
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(match scroll {
            Scroll::Lines(n) => GridScroll::Delta(n),
            Scroll::Top => GridScroll::Top,
            Scroll::Bottom => GridScroll::Bottom,
        });
    }

    /// The view shows history, not the live screen.
    pub fn scrolled_back(&self) -> bool {
        self.term.grid().display_offset() != 0
    }

    /// The screen as the view shows it: the live screen, or history lines when the view
    /// is scrolled back (then without a cursor).
    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let (rows, cols) = (grid.screen_lines(), grid.columns());
        let offset = grid.display_offset();
        let lines = (0..rows)
            .map(|l| {
                let row = &grid[Line(l as i32 - offset as i32)];
                (0..cols).map(|c| convert::cell(&row[Column(c)])).collect()
            })
            .collect();
        let p = grid.cursor.point;
        let mut modes = self.modes();
        modes.show_cursor &= offset == 0;
        Snapshot {
            cols: cols as u16,
            rows: rows as u16,
            lines,
            cursor: Cursor {
                row: p.line.0.max(0) as u16,
                col: p.column.0 as u16,
            },
            modes,
            scroll: ScrollPos {
                offset: offset as u32,
                history: grid.history_size() as u32,
            },
        }
    }

    /// A place in the history as alacritty counts it: the live screen's top is line 0.
    fn point(&self, at: Pos) -> Point {
        let grid = self.term.grid();
        let top = -(grid.history_size() as i32);
        let last = grid.screen_lines() as i32 - 1;
        let line = (top + at.line as i32).clamp(top, last);
        let col = (at.col as usize).min(grid.columns().saturating_sub(1));
        Point::new(Line(line), Column(col))
    }

    fn pos(&self, p: Point) -> Pos {
        let history = self.term.grid().history_size() as i32;
        Pos {
            line: (p.line.0 + history).max(0) as u32,
            col: p.column.0 as u16,
        }
    }

    /// The text from `a` to `b` (either way round), whole lines when `lines`.
    pub fn text(&self, a: Pos, b: Pos, lines: bool) -> String {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let (mut start, mut end) = (self.point(a), self.point(b));
        if lines {
            start.column = Column(0);
            end.column = self.term.grid().last_column();
        }
        self.term.bounds_to_string(start, end)
    }

    /// The next place `query` (text, not a pattern; any case unless it has a capital)
    /// is from `from`, up or down, skipping one that starts there; with which of how
    /// many in the whole history it is (1-based; 0 when none).
    pub fn search(&self, query: &str, from: Pos, backward: bool) -> (Option<(Pos, Pos)>, u32, u32) {
        let Ok(mut regex) = RegexSearch::new(&escape(query)) else {
            return (None, 0, 0);
        };
        let grid = self.term.grid();
        let first = Point::new(Line(-(grid.history_size() as i32)), Column(0));
        let last = Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column());
        let all: Vec<Match> =
            RegexIter::new(first, last, Direction::Right, &self.term, &mut regex).collect();
        let origin = self.point(from);
        let found = if backward {
            all.iter().rev().find(|m| *m.start() < origin)
        } else {
            all.iter().find(|m| *m.start() > origin)
        };
        match found {
            Some(m) => {
                let index = all.iter().position(|x| x == m).map_or(0, |i| i as u32 + 1);
                (
                    Some((self.pos(*m.start()), self.pos(*m.end()))),
                    index,
                    all.len() as u32,
                )
            }
            None => (None, 0, all.len() as u32),
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
        let colors = &self.cfg.colors;
        match index {
            256 | 258 => rgb(colors.fg), // foreground, cursor
            257 => rgb(colors.bg),       // background
            i if i < 16 && colors.ansi.is_some() => rgb(colors.ansi.unwrap()[i]),
            i => convert::xterm_rgb(i),
        }
    }
}

/// `text` as a pattern that finds only itself.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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

    /// Lines "1" to "n", each on its own row.
    fn numbered(t: &mut TermCore, n: usize) {
        let text: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        t.feed(text.join("\r\n").as_bytes());
    }

    #[test]
    fn text_is_cut_from_the_history_and_the_screen_alike() {
        let mut t = core();
        numbered(&mut t, 12);
        let at = |line, col| Pos { line, col };
        // Lines 0 to 6 are history ("1" to "7"), 7 to 11 the screen.
        assert_eq!(
            t.text(at(5, 3), at(8, 0), true),
            "6\n7\n8\n9",
            "whole lines"
        );
        assert_eq!(
            t.text(at(1, 0), at(0, 0), false),
            "1\n2",
            "either way round"
        );
        t.feed(b"\r\nhello world");
        assert_eq!(t.text(at(12, 6), at(12, 10), false), "world");
    }

    #[test]
    fn a_search_goes_up_or_down_and_says_which_of_how_many() {
        let mut t = core();
        let lines = [
            "error one",
            "ok",
            "Error two",
            "ok",
            "ok",
            "x error three",
            "ok",
            "ok",
        ];
        t.feed(lines.join("\r\n").as_bytes());
        let at = |line, col| Pos { line, col };
        // From the last line up: the nearest first.
        let (found, index, total) = t.search("error", at(7, 0), true);
        assert_eq!((found, index, total), (Some((at(5, 2), at(5, 6))), 3, 3));
        let (found, index, _) = t.search("error", at(5, 2), true);
        assert_eq!(
            (found, index),
            (Some((at(2, 0), at(2, 4))), 2),
            "past the one it is on"
        );
        let (found, index, _) = t.search("error", at(0, 0), false);
        assert_eq!((found, index), (Some((at(2, 0), at(2, 4))), 2), "down");
        // A capital asks for that case; a pattern is only text.
        assert_eq!(t.search("Error", at(7, 0), true).2, 1);
        assert_eq!(t.search("e.ror", at(7, 0), true), (None, 0, 0));
    }

    #[test]
    fn scrolling_back_shows_the_history_and_hides_the_cursor() {
        let mut t = core();
        numbered(&mut t, 12);
        let live = t.snapshot();
        assert_eq!(live.line_text(4), "12");
        assert_eq!(
            live.scroll,
            ScrollPos {
                offset: 0,
                history: 7
            }
        );
        assert!(live.modes.show_cursor);

        t.scroll(Scroll::Lines(3));
        let s = t.snapshot();
        assert_eq!(s.line_text(0), "5");
        assert_eq!(s.line_text(4), "9");
        assert_eq!(
            s.scroll,
            ScrollPos {
                offset: 3,
                history: 7
            }
        );
        assert!(!s.modes.show_cursor, "no cursor over the history");

        t.scroll(Scroll::Top);
        assert_eq!(t.snapshot().line_text(0), "1");
        t.scroll(Scroll::Lines(100));
        assert_eq!(t.snapshot().scroll.offset, 7, "the oldest line stops it");
        t.scroll(Scroll::Lines(-2));
        assert_eq!(t.snapshot().scroll.offset, 5);
        t.scroll(Scroll::Bottom);
        assert_eq!(t.snapshot(), live);
    }

    #[test]
    fn a_scrolled_view_stays_put_while_output_comes() {
        let mut t = core();
        numbered(&mut t, 12);
        t.scroll(Scroll::Lines(3));
        t.feed(b"\r\n13\r\n14");
        let s = t.snapshot();
        assert_eq!(s.line_text(0), "5", "the same lines as before the output");
        assert_eq!(s.scroll.offset, 5);
        t.scroll(Scroll::Bottom);
        assert_eq!(t.snapshot().line_text(4), "14");
    }

    #[test]
    fn a_full_screen_program_has_no_history_to_scroll() {
        let mut t = core();
        numbered(&mut t, 12);
        t.feed(b"\x1b[?1049h\x1b[Hfull screen");
        t.scroll(Scroll::Lines(3));
        let s = t.snapshot();
        assert_eq!(s.line_text(0), "full screen");
        assert_eq!(s.scroll, ScrollPos::default());
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
    fn colour_queries_get_a_dark_terminal_until_told_otherwise() {
        let mut t = core();
        let r = replies(&t.feed(b"\x1b]11;?\x07"));
        assert_eq!(r, vec![b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07".to_vec()]);
    }

    #[test]
    fn colour_queries_get_the_colours_set_later() {
        let mut t = core();
        t.set_colors(TermColors {
            fg: (0x2b, 0x25, 0x30),
            bg: (0xfb, 0xf4, 0xe8),
            ansi: Some([(0xc0, 0x39, 0x2b); 16]),
        });
        let r = replies(&t.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;1;?\x07\x1b]4;196;?\x07"));
        assert_eq!(
            r,
            vec![
                b"\x1b]10;rgb:2b2b/2525/3030\x07".to_vec(),
                b"\x1b]11;rgb:fbfb/f4f4/e8e8\x07".to_vec(),
                b"\x1b]4;1;rgb:c0c0/3939/2b2b\x07".to_vec(),
                b"\x1b]4;196;rgb:ffff/0000/0000\x07".to_vec(),
            ]
        );
    }

    #[test]
    fn a_colour_the_child_set_itself_wins_over_the_theme() {
        let mut t = core();
        t.set_colors(TermColors {
            ansi: Some([(0, 0, 0); 16]),
            ..TermColors::default()
        });
        t.feed(b"\x1b]4;1;rgb:12/34/56\x07");
        let r = replies(&t.feed(b"\x1b]4;1;?\x07"));
        assert_eq!(r, vec![b"\x1b]4;1;rgb:1212/3434/5656\x07".to_vec()]);
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
