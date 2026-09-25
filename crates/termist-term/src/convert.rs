use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell as ACell, Flags};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Rgb};
use termist_core::{Cell, Color, Modes, cell_flags};

pub fn cell(c: &ACell) -> Cell {
    let f = c.flags;
    let mut flags = 0;
    for (a, b) in [
        (Flags::BOLD, cell_flags::BOLD),
        (Flags::ITALIC, cell_flags::ITALIC),
        (Flags::INVERSE, cell_flags::INVERSE),
        (Flags::DIM, cell_flags::DIM),
        (Flags::HIDDEN, cell_flags::HIDDEN),
        (Flags::STRIKEOUT, cell_flags::STRIKEOUT),
        (Flags::WIDE_CHAR, cell_flags::WIDE),
        (Flags::WIDE_CHAR_SPACER, cell_flags::WIDE_SPACER),
    ] {
        if f.contains(a) {
            flags |= b;
        }
    }
    if f.intersects(Flags::ALL_UNDERLINES) {
        flags |= cell_flags::UNDERLINE;
    }
    Cell {
        ch: if c.c == '\0' { ' ' } else { c.c },
        fg: color(c.fg),
        bg: color(c.bg),
        flags,
    }
}

pub fn color(c: AColor) -> Color {
    match c {
        AColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Named(n) => named(n),
    }
}

fn named(n: NamedColor) -> Color {
    let i = n as i32;
    match i {
        0..=15 => Color::Indexed(i as u8),
        259..=266 => Color::Indexed((i - 259) as u8), // DimBlack..DimWhite
        _ => Color::Default,                          // foreground, background, cursor, …
    }
}

pub fn modes(m: TermMode) -> Modes {
    Modes {
        alt_screen: m.contains(TermMode::ALT_SCREEN),
        app_cursor: m.contains(TermMode::APP_CURSOR),
        bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
        mouse_reporting: m.intersects(TermMode::MOUSE_MODE),
        sgr_mouse: m.contains(TermMode::SGR_MOUSE),
        focus_events: m.contains(TermMode::FOCUS_IN_OUT),
        show_cursor: m.contains(TermMode::SHOW_CURSOR),
    }
}

/// xterm's default 256-colour palette, for colour queries about indices the child never set.
pub fn xterm_rgb(i: usize) -> Rgb {
    const BASE16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    let (r, g, b) = match i {
        0..=15 => BASE16[i],
        16..=231 => {
            let i = i - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = (8 + (i - 232) * 10) as u8;
            (v, v, v)
        }
        _ => (0, 0, 0),
    };
    Rgb { r, g, b }
}
