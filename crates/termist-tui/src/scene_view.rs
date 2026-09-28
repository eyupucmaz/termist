//! Drawing an Istanbul scene: on the splash, the empty grid, the idle screen and the
//! help. A scene that does not fit, or a terminal of 16 colours, gets the wordmark.
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};
use termist_scenes::{Scene, TimeOfDay};

pub const WORDMARK: &str = "termist · terminal istanbul";

/// Frames a second.
pub const FPS: u64 = 10;

/// The frame to draw `elapsed` after a scene came up; always the first without
/// animations.
pub fn frame_number(elapsed: Duration, animations: bool) -> u64 {
    if animations {
        elapsed.as_millis() as u64 * FPS / 1000
    } else {
        0
    }
}

/// A scene on screen, since when, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Showing {
    pub name: &'static str,
    pub since: Instant,
    pub kind: ShowKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowKind {
    /// At start, over everything, for a second at most.
    Splash,
    /// After a while without keys, over the body; the header stays.
    Idle,
}

/// Whether a scene fits in `area` with `below` lines under it.
pub fn fits(scene: &Scene, area: Rect, below: u16) -> bool {
    area.width as usize >= scene.width && area.height as usize >= scene.height + below as usize
}

/// Draws `scene` centred in `area` with `title` and `caption` under it, or the
/// wordmark and the caption when it does not fit or the terminal cannot draw it.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    scene: &Scene,
    tod: TimeOfDay,
    n: u64,
    theme: &Theme,
    title: Line<'_>,
    caption: &[Line<'_>],
) {
    let below = caption.len() as u16 + 2;
    if !theme.draws_scenes() || !fits(scene, area, below) {
        wordmark(buf, area, theme, caption);
        return;
    }
    let caption: Vec<&Line<'_>> = std::iter::once(&title).chain(caption).collect();
    let (w, h) = (scene.width as u16, scene.height as u16);
    let top = area.y + (area.height - h - below) / 2;
    let left = area.x + (area.width - w) / 2;
    for (y, row) in scene.frame(tod, n).iter().enumerate() {
        for (x, c) in row.iter().enumerate() {
            let style = Style::default().fg(theme.rgb(c.fg)).bg(theme.rgb(c.bg));
            buf[(left + x as u16, top + y as u16)]
                .set_char(c.ch)
                .set_style(style);
        }
    }
    for (i, line) in caption.iter().enumerate() {
        let y = top + h + 1 + i as u16;
        let x = left + w.saturating_sub(line.width() as u16) / 2;
        buf.set_line(x, y, line, w);
    }
}

/// `termist · terminal istanbul`, centred, with `caption` under it.
pub fn wordmark(buf: &mut Buffer, area: Rect, theme: &Theme, caption: &[Line<'_>]) {
    if area.height == 0 {
        return;
    }
    let lines = caption.len() as u16 + 2;
    let top = area.y + area.height.saturating_sub(lines) / 2;
    let mark = Line::from(Span::styled(
        WORDMARK,
        theme.accent.add_modifier(Modifier::BOLD),
    ));
    let centre = |line: &Line<'_>| area.x + area.width.saturating_sub(line.width() as u16) / 2;
    buf.set_line(centre(&mark), top, &mark, area.width);
    for (i, line) in caption.iter().enumerate() {
        let y = top + 2 + i as u16;
        if y < area.bottom() {
            buf.set_line(centre(line), y, line, area.width);
        }
    }
}

/// The scene as lines of text, for the help screen.
pub fn lines(scene: &Scene, tod: TimeOfDay, n: u64, theme: &Theme) -> Vec<Line<'static>> {
    scene
        .frame(tod, n)
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|c| {
                        Span::styled(
                            c.ch.to_string(),
                            Style::default().fg(theme.rgb(c.fg)).bg(theme.rgb(c.bg)),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::config::ColorDepth;

    fn text(buf: &Buffer) -> String {
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn a_scene_that_fits_is_drawn_and_one_that_does_not_is_the_wordmark() {
        let scene = Scene::load("galata").unwrap();
        let theme = Theme::named("uskudar", ColorDepth::TrueColor);
        let caption = [Line::from("a hint")];
        let title = || Line::from("Galata Kulesi");
        let mut big = Buffer::empty(Rect::new(0, 0, 100, 30));
        let a = big.area;
        draw(
            &mut big,
            a,
            &scene,
            TimeOfDay::Gece,
            0,
            &theme,
            title(),
            &caption,
        );
        assert!(!text(&big).contains(WORDMARK));
        assert!(text(&big).contains("Galata Kulesi"));
        assert!(text(&big).contains("a hint"));
        assert!(
            matches!(big[(2, 2)].bg, ratatui::style::Color::Rgb(..)),
            "the night sky"
        );
        let mut small = Buffer::empty(Rect::new(0, 0, 80, 24));
        let a = small.area;
        draw(
            &mut small,
            a,
            &scene,
            TimeOfDay::Gece,
            0,
            &theme,
            title(),
            &caption,
        );
        assert!(text(&small).contains(WORDMARK));
        assert!(
            !text(&small).contains("Galata Kulesi"),
            "no title without the scene"
        );
        assert!(text(&small).contains("a hint"));
    }

    #[test]
    fn with_16_colours_the_wordmark_stands_in() {
        let scene = Scene::load("galata").unwrap();
        let theme = Theme::named("uskudar", ColorDepth::Ansi16);
        let mut big = Buffer::empty(Rect::new(0, 0, 120, 40));
        let a = big.area;
        draw(
            &mut big,
            a,
            &scene,
            TimeOfDay::Gece,
            0,
            &theme,
            Line::default(),
            &[],
        );
        assert!(text(&big).contains(WORDMARK));
    }

    #[test]
    fn frames_follow_the_clock_unless_animations_are_off() {
        assert_eq!(frame_number(Duration::from_millis(1250), true), 12);
        assert_eq!(frame_number(Duration::from_millis(1250), false), 0);
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let scene = Scene::load("vapur").unwrap();
        let theme = Theme::terminal();
        for (w, h) in [(0, 0), (1, 1), (30, 2), (96, 26)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
            let area = Rect::new(0, 0, w, h);
            let x = [Line::from("x")];
            draw(
                &mut buf,
                area,
                &scene,
                TimeOfDay::Sabah,
                3,
                &theme,
                Line::default(),
                &x,
            );
        }
    }
}
