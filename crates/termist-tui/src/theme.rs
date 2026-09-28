//! Themes: the colours of termist's own screen and of the agents' default colours.
//!
//! A theme is data (`assets/themes/<id>.toml`, built in). It names roles, not places:
//! `dim` text, the `accent` of the selected card, one colour per agent status, and the
//! 16 ANSI colours agents print with. Status colours keep their meaning in every theme.
use ratatui::style::{Color, Modifier, Style};
use termist_core::config::ColorDepth;
use termist_core::{AgentStatus, TermColors};
use toml::{Table, Value};

pub type Rgb = (u8, u8, u8);

const SOURCES: [(&str, &str); 3] = [
    (
        "uskudar",
        include_str!("../../../assets/themes/uskudar.toml"),
    ),
    ("moda", include_str!("../../../assets/themes/moda.toml")),
    (
        "terminal",
        include_str!("../../../assets/themes/terminal.toml"),
    ),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: &'static str,
    pub name: String,
    /// The whole screen: `Style::default()` when the theme paints nothing.
    pub base: Style,
    pub dim: Style,
    pub border: Style,
    pub accent: Style,
    pub focus: Style,
    pub warn: Style,
    pub error: Style,
    /// The "archive" label of the archive view.
    pub archive: Style,
    pub selection: Style,
    pub tab_active: Style,
    status: [Color; 8],
    /// Pane cells in the agent's default colours.
    pub pane_fg: Color,
    pub pane_bg: Color,
    /// ANSI colours 0-15 in the pane; `None` leaves them to the host terminal.
    ansi: Option<[Color; 16]>,
    /// What agents are told their terminal's colours are, in 24-bit colour (the pane
    /// draws exactly these); `None` for a theme that paints nothing.
    pub agent_colors: Option<TermColors>,
    /// The theme asked for could not be drawn with this terminal's colours, and this
    /// one stands in for it.
    pub stands_in_for: Option<&'static str>,
}

impl Theme {
    /// The built-in theme `id` drawn at `depth` (`Auto` means 24-bit). A painting
    /// theme needs at least 256 colours; with 16 the terminal theme stands in.
    /// An unknown id is the terminal theme.
    pub fn named(id: &str, depth: ColorDepth) -> Theme {
        let (id, source) = SOURCES
            .iter()
            .copied()
            .find(|(i, _)| *i == id)
            .unwrap_or(SOURCES[2]);
        let spec = Spec::parse(source).unwrap_or_else(|e| panic!("theme {id}: {e}"));
        if depth == ColorDepth::Ansi16 && spec.paints() {
            let mut theme = Theme::named("terminal", depth);
            theme.stands_in_for = Some(id);
            return theme;
        }
        spec.resolve(id, depth)
    }

    /// The host terminal's own colours, as today: the theme tests and a bare `App` use.
    pub fn terminal() -> Theme {
        Theme::named("terminal", ColorDepth::TrueColor)
    }

    pub fn status(&self, status: AgentStatus) -> Color {
        self.status[status_index(status)]
    }

    /// A pane cell's colour: the agent's default and ANSI 0-15 through the theme,
    /// anything else as the agent wrote it.
    pub fn pane_color(&self, c: termist_core::Color, fg: bool) -> Color {
        match c {
            termist_core::Color::Default if fg => self.pane_fg,
            termist_core::Color::Default => self.pane_bg,
            termist_core::Color::Indexed(i) => match &self.ansi {
                Some(ansi) if i < 16 => ansi[i as usize],
                _ => Color::Indexed(i),
            },
            termist_core::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

fn status_index(status: AgentStatus) -> usize {
    match status {
        AgentStatus::Fresh => 0,
        AgentStatus::Running => 1,
        AgentStatus::Unseen => 2,
        AgentStatus::Finished => 3,
        AgentStatus::NeedsFeedback => 4,
        AgentStatus::Exited { code: Some(0) } => 5,
        AgentStatus::Exited { .. } => 6,
        AgentStatus::Disconnected => 7,
    }
}

const STATUS_KEYS: [&str; 8] = [
    "fresh",
    "running",
    "done",
    "ready",
    "waiting",
    "closed",
    "exited",
    "disconnected",
];

/// A colour as written in a theme file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Paint {
    Rgb(Rgb),
    Named(Color),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StyleSpec {
    fg: Option<Paint>,
    bg: Option<Paint>,
    bold: bool,
    reversed: bool,
}

/// A theme file, read but not yet fitted to a terminal.
#[derive(Clone, Debug, PartialEq)]
struct Spec {
    name: String,
    bg: Option<Rgb>,
    fg: Option<Rgb>,
    dim: StyleSpec,
    border: StyleSpec,
    accent: StyleSpec,
    focus: StyleSpec,
    warn: StyleSpec,
    error: StyleSpec,
    archive: StyleSpec,
    selection: StyleSpec,
    tab_active: StyleSpec,
    status: [Paint; 8],
    ansi: Option<[Rgb; 16]>,
}

impl Spec {
    fn parse(source: &str) -> Result<Spec, String> {
        let t: Table = source.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or("no name")?
            .to_string();
        let empty = Table::new();
        let table = |key: &str| t.get(key).and_then(Value::as_table).unwrap_or(&empty);
        let ui = table("ui");
        let style = |key: &str| -> Result<StyleSpec, String> {
            ui.get(key)
                .map(|v| style_spec(v).map_err(|e| format!("ui.{key}: {e}")))
                .unwrap_or_else(|| Err(format!("ui.{key} is missing")))
        };
        let rgb_of = |key: &str| -> Result<Option<Rgb>, String> {
            ui.get(key)
                .map(|v| {
                    v.as_str()
                        .and_then(hex)
                        .ok_or(format!("ui.{key} should be \"#rrggbb\""))
                })
                .transpose()
        };
        let status_table = table("status");
        let mut status = [Paint::Named(Color::Reset); 8];
        for (slot, key) in status.iter_mut().zip(STATUS_KEYS) {
            *slot = status_table
                .get(key)
                .and_then(Value::as_str)
                .and_then(paint)
                .ok_or(format!("status.{key} is missing or not a colour"))?;
        }
        let ansi = match table("ansi").get("colors") {
            None => None,
            Some(v) => {
                let list = v.as_array().ok_or("ansi.colors should be a list")?;
                let colors: Vec<Rgb> = list
                    .iter()
                    .map(|c| c.as_str().and_then(hex))
                    .collect::<Option<_>>()
                    .ok_or("ansi.colors should be \"#rrggbb\" colours")?;
                Some(
                    colors
                        .try_into()
                        .map_err(|_| "ansi.colors should have 16 colours")?,
                )
            }
        };
        let spec = Spec {
            name,
            bg: rgb_of("bg")?,
            fg: rgb_of("fg")?,
            dim: style("dim")?,
            border: style("border")?,
            accent: style("accent")?,
            focus: style("focus")?,
            warn: style("warn")?,
            error: style("error")?,
            archive: style("archive")?,
            selection: style("selection")?,
            tab_active: style("tab_active")?,
            status,
            ansi,
        };
        if spec.bg.is_some() != spec.fg.is_some() || spec.bg.is_some() != spec.ansi.is_some() {
            return Err("a theme that paints needs bg, fg and ansi.colors together".into());
        }
        Ok(spec)
    }

    fn paints(&self) -> bool {
        self.bg.is_some()
    }

    fn resolve(&self, id: &'static str, depth: ColorDepth) -> Theme {
        let color = |p: Paint| match p {
            Paint::Named(c) => c,
            Paint::Rgb(rgb) => fit(rgb, depth),
        };
        let style = |s: StyleSpec| {
            let mut style = Style::default();
            if let Some(fg) = s.fg {
                style = style.fg(color(fg));
            }
            if let Some(bg) = s.bg {
                style = style.bg(color(bg));
            }
            if s.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if s.reversed {
                style = style.add_modifier(Modifier::REVERSED);
            }
            style
        };
        let pane_fg = self.fg.map_or(Color::Reset, |c| fit(c, depth));
        let pane_bg = self.bg.map_or(Color::Reset, |c| fit(c, depth));
        let base = match (self.fg, self.bg) {
            (Some(_), Some(_)) => Style::default().fg(pane_fg).bg(pane_bg),
            _ => Style::default(),
        };
        let agent_colors = match (self.fg, self.bg, self.ansi) {
            (Some(fg), Some(bg), Some(ansi)) => Some(TermColors {
                fg,
                bg,
                ansi: Some(ansi),
            }),
            _ => None,
        };
        Theme {
            id,
            name: self.name.clone(),
            base,
            dim: style(self.dim),
            border: style(self.border),
            accent: style(self.accent),
            focus: style(self.focus),
            warn: style(self.warn),
            error: style(self.error),
            archive: style(self.archive),
            selection: style(self.selection),
            tab_active: style(self.tab_active),
            status: self.status.map(color),
            pane_fg,
            pane_bg,
            ansi: self.ansi.map(|a| a.map(|c| fit(c, depth))),
            agent_colors,
            stands_in_for: None,
        }
    }
}

fn style_spec(v: &Value) -> Result<StyleSpec, String> {
    if let Some(s) = v.as_str() {
        return Ok(StyleSpec {
            fg: Some(paint(s).ok_or(format!("\"{s}\" is not a colour"))?),
            ..StyleSpec::default()
        });
    }
    let t = v.as_table().ok_or("should be a colour or a table")?;
    let colour = |key: &str| {
        t.get(key)
            .map(|c| {
                c.as_str()
                    .and_then(paint)
                    .ok_or(format!("{key} is not a colour"))
            })
            .transpose()
    };
    let flag = |key: &str| t.get(key).and_then(Value::as_bool).unwrap_or(false);
    Ok(StyleSpec {
        fg: colour("fg")?,
        bg: colour("bg")?,
        bold: flag("bold"),
        reversed: flag("reversed"),
    })
}

fn paint(s: &str) -> Option<Paint> {
    if let Some(rgb) = hex(s) {
        return Some(Paint::Rgb(rgb));
    }
    let named = match s {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "dark_gray" => Color::DarkGray,
        "white" => Color::White,
        _ => return None,
    };
    Some(Paint::Named(named))
}

fn hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// `rgb` as this terminal can draw it: itself with 24-bit colour, else the nearest
/// of the xterm 256 colours.
fn fit(rgb: Rgb, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::Auto | ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        ColorDepth::Ansi256 | ColorDepth::Ansi16 => Color::Indexed(nearest_256(rgb)),
    }
}

/// The nearest colour of the 6×6×6 cube (16-231) or the grey ramp (232-255). The
/// first 16 are left out: terminals redefine them.
pub fn nearest_256((r, g, b): Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let dist = |(x, y, z): Rgb| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(r, x) + d(g, y) + d(b, z)
    };
    let level = |c: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] as i32 - c as i32).abs())
            .unwrap()
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let grey_step = (0..24)
        .min_by_key(|&i| {
            let v = 8 + 10 * i as u8;
            dist((v, v, v))
        })
        .unwrap();
    let grey = 8 + 10 * grey_step as u8;
    if dist((grey, grey, grey)) < dist(cube) {
        232 + grey_step as u8
    } else {
        cube_index as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> Spec {
        let source = SOURCES.iter().find(|(i, _)| *i == id).unwrap().1;
        Spec::parse(source).unwrap()
    }

    fn luminance((r, g, b): Rgb) -> f64 {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn rgb(p: Option<Paint>) -> Rgb {
        match p {
            Some(Paint::Rgb(c)) => c,
            other => panic!("expected a #rrggbb colour, got {other:?}"),
        }
    }

    #[test]
    fn every_built_in_theme_reads() {
        for (id, _) in SOURCES {
            let theme = Theme::named(id, ColorDepth::TrueColor);
            assert_eq!(theme.id, id);
            assert!(!theme.name.is_empty());
        }
        assert_eq!(
            Theme::named("uskudar", ColorDepth::TrueColor).name,
            "Üsküdar"
        );
    }

    /// Text must be readable on the theme's background, and every status must stand
    /// out from it (WCAG contrast ratios).
    #[test]
    fn painting_themes_are_readable() {
        for id in ["uskudar", "moda"] {
            let s = spec(id);
            let bg = s.bg.unwrap();
            let mut low = vec![];
            let mut check = |what: String, c: Rgb, on: Rgb, min: f64| {
                let ratio = contrast(c, on);
                if ratio < min {
                    low.push(format!("{id} {what}: {ratio:.2} < {min}"));
                }
            };
            check("fg".into(), s.fg.unwrap(), bg, 4.5);
            for (what, st) in [
                ("dim", s.dim),
                ("accent", s.accent),
                ("focus", s.focus),
                ("warn", s.warn),
                ("error", s.error),
                ("archive", s.archive),
            ] {
                check(what.into(), rgb(st.fg), bg, 3.0);
            }
            check("border".into(), rgb(s.border.fg), bg, 1.5);
            for (what, st) in [("selection", s.selection), ("tab_active", s.tab_active)] {
                check(what.into(), rgb(st.fg), rgb(st.bg), 4.5);
            }
            for (key, p) in STATUS_KEYS.iter().zip(s.status) {
                check(format!("status.{key}"), rgb(Some(p)), bg, 3.0);
            }
            // A light theme's white and a dark theme's black are meant to be faint.
            let light = luminance(bg) > 0.5;
            let exempt: [usize; 2] = if light { [7, 15] } else { [0, 8] };
            for (i, c) in s.ansi.unwrap().into_iter().enumerate() {
                if !exempt.contains(&i) && ![0, 7, 8, 15].contains(&i) {
                    check(format!("ansi {i}"), c, bg, 3.0);
                }
            }
            assert!(low.is_empty(), "{low:#?}");
        }
    }

    #[test]
    fn a_painting_theme_paints_the_screen_and_the_pane() {
        let t = Theme::named("moda", ColorDepth::TrueColor);
        assert_eq!(t.base.bg, Some(Color::Rgb(0xfb, 0xf4, 0xe8)));
        assert_eq!(
            t.pane_color(termist_core::Color::Default, false),
            t.base.bg.unwrap()
        );
        assert_eq!(
            t.pane_color(termist_core::Color::Indexed(1), true),
            Color::Rgb(0xc0, 0x39, 0x2b),
            "ANSI red is the theme's red"
        );
        assert_eq!(
            t.pane_color(termist_core::Color::Indexed(196), true),
            Color::Indexed(196)
        );
        assert_eq!(
            t.pane_color(termist_core::Color::Rgb(1, 2, 3), true),
            Color::Rgb(1, 2, 3)
        );
        let agents = t.agent_colors.unwrap();
        assert_eq!(agents.bg, (0xfb, 0xf4, 0xe8));
        assert_eq!(agents.ansi.unwrap()[1], (0xc0, 0x39, 0x2b));
    }

    #[test]
    fn the_terminal_theme_is_todays_look() {
        let t = Theme::terminal();
        assert_eq!(t.base, Style::default());
        assert_eq!(t.status(AgentStatus::Running), Color::Yellow);
        assert_eq!(t.status(AgentStatus::NeedsFeedback), Color::Red);
        assert_eq!(t.status(AgentStatus::Disconnected), Color::Gray);
        assert_eq!(
            t.status(AgentStatus::Exited { code: Some(0) }),
            Color::DarkGray
        );
        assert_eq!(t.accent.fg, Some(Color::Cyan));
        assert!(t.selection.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(
            t.pane_color(termist_core::Color::Default, true),
            Color::Reset
        );
        assert_eq!(
            t.pane_color(termist_core::Color::Indexed(1), true),
            Color::Indexed(1)
        );
        assert_eq!(t.agent_colors, None);
    }

    #[test]
    fn with_256_colours_hex_becomes_the_nearest_xterm_colour() {
        assert_eq!(nearest_256((255, 0, 0)), 196);
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((128, 128, 128)), 244);
        assert_eq!(nearest_256((0x0f, 0x1d, 0x2e)), 234, "Üsküdar's ground");
        let t = Theme::named("uskudar", ColorDepth::Ansi256);
        assert!(matches!(t.base.bg, Some(Color::Indexed(_))));
        assert!(matches!(
            t.pane_color(termist_core::Color::Indexed(1), true),
            Color::Indexed(i) if i >= 16
        ));
        assert!(
            t.agent_colors.is_some(),
            "agents are still told the 24-bit colours"
        );
    }

    #[test]
    fn with_16_colours_the_terminal_theme_stands_in() {
        let t = Theme::named("moda", ColorDepth::Ansi16);
        assert_eq!(t.id, "terminal");
        assert_eq!(t.stands_in_for, Some("moda"));
        assert_eq!(
            Theme::named("terminal", ColorDepth::Ansi16).stands_in_for,
            None
        );
    }

    #[test]
    fn an_unknown_id_is_the_terminal_theme() {
        assert_eq!(Theme::named("nope", ColorDepth::TrueColor).id, "terminal");
    }

    #[test]
    fn a_broken_theme_file_says_what_is_wrong() {
        let src = SOURCES[0].1.replace("dim = \"#93a4b8\"", "dim = \"#12\"");
        let err = Spec::parse(&src).unwrap_err();
        assert!(err.contains("ui.dim"), "{err}");
        let err = Spec::parse("name = \"x\"\n").unwrap_err();
        assert!(err.contains("status.fresh"), "{err}");
        let src = SOURCES[0].1.replace("\"#ffffff\",\n]", "]");
        let err = Spec::parse(&src).unwrap_err();
        assert!(err.contains("16 colours"), "{err}");
    }
}
