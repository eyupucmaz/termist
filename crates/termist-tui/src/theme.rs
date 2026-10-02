//! Themes: the colours of termist's own screen and of the agents' default colours.
//!
//! A theme is data (`assets/themes/<id>.toml`, built in). It names roles, not places:
//! `dim` text, the `accent` of the selected card, one colour per agent status, and the
//! 16 ANSI colours agents print with. Status colours keep their meaning in every theme.
use ratatui::style::{Color, Modifier, Style};
use std::path::{Path, PathBuf};
use termist_core::config::{ColorDepth, Problem};
use termist_core::{AgentStatus, TermColors};
use toml::{Table, Value};

pub type Rgb = (u8, u8, u8);

const USKUDAR: &str = include_str!("../../../assets/themes/uskudar.toml");
const TERMINAL: &str = include_str!("../../../assets/themes/terminal.toml");

/// The built-in themes, in the order the settings go through them.
const BUILTIN: [(&str, &str); 3] = [
    ("uskudar", USKUDAR),
    ("moda", include_str!("../../../assets/themes/moda.toml")),
    ("terminal", TERMINAL),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: String,
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
    pub stands_in_for: Option<String>,
    /// The colours this terminal draws.
    pub depth: ColorDepth,
}

impl Theme {
    /// The built-in theme `id` drawn at `depth`; an unknown one is Üsküdar.
    pub fn named(id: &str, depth: ColorDepth) -> Theme {
        Themes::builtin().get(id, depth)
    }

    /// The host terminal's own colours, as today: the theme tests and a bare `App` use.
    pub fn terminal() -> Theme {
        Theme::named("terminal", ColorDepth::TrueColor)
    }

    /// A 24-bit colour as this terminal can draw it.
    pub fn rgb(&self, rgb: Rgb) -> Color {
        fit(rgb, self.depth)
    }

    /// Whether this terminal can draw the scenes: 256 colours at least.
    pub fn draws_scenes(&self) -> bool {
        self.depth != ColorDepth::Ansi16
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
        Spec::from_table(&t)
    }

    fn from_table(t: &Table) -> Result<Spec, String> {
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

    /// A user's theme file: what it leaves out comes from Üsküdar, and without a name
    /// it is called by its id.
    fn user(id: &str, text: &str) -> Result<Spec, String> {
        let mine: Table = text
            .parse()
            .map_err(|e: toml::de::Error| e.message().to_string())?;
        let mut table: Table = USKUDAR.parse().expect("Üsküdar is valid");
        table.insert("name".into(), Value::String(id.into()));
        merge(&mut table, mine);
        Spec::from_table(&table)
    }

    fn paints(&self) -> bool {
        self.bg.is_some()
    }

    fn resolve(&self, id: &str, depth: ColorDepth) -> Theme {
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
            id: id.to_string(),
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
            depth,
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

/// `over`'s values win; tables merge key by key.
fn merge(base: &mut Table, over: Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(b)), Value::Table(o)) => merge(b, o),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// A theme as written: built in, or a file in the themes folder.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    id: String,
    spec: Spec,
}

/// Every theme there is to draw, in the order the settings go through them: the
/// built-in ones, then the user's own.
#[derive(Clone, Debug, PartialEq)]
pub struct Themes {
    sources: Vec<Source>,
}

impl Default for Themes {
    fn default() -> Self {
        Themes::builtin()
    }
}

impl Themes {
    pub fn builtin() -> Themes {
        let sources = BUILTIN
            .iter()
            .map(|(id, text)| Source {
                id: id.to_string(),
                spec: Spec::parse(text).unwrap_or_else(|e| panic!("theme {id}: {e}")),
            })
            .collect();
        Themes { sources }
    }

    /// The built-in themes and the `*.toml` files in `dir`. A file's id is its name; it
    /// takes the place of a built-in theme of that id. A file that cannot be used is
    /// left out and reported.
    pub fn load(dir: &Path) -> (Themes, Vec<Problem>) {
        let mut themes = Themes::builtin();
        let mut problems = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (themes, problems);
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for path in files {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let spec = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| Spec::user(&id, &text));
            match spec {
                Ok(spec) => themes.put(Source { id, spec }),
                Err(e) => problems.push(Problem {
                    path: path.display().to_string(),
                    message: format!("theme not used: {e}"),
                }),
            }
        }
        (themes, problems)
    }

    fn put(&mut self, source: Source) {
        match self.sources.iter_mut().find(|s| s.id == source.id) {
            Some(s) => *s = source,
            None => self.sources.push(source),
        }
    }

    fn find(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(|s| s.id.as_str())
    }

    /// The name theme `id` shows; `id` itself for an unknown one.
    pub fn name_of(&self, id: &str) -> String {
        self.find(id)
            .map_or_else(|| id.to_string(), |s| s.spec.name.clone())
    }

    /// `theme = "<id>"` naming a theme that is not there.
    pub fn check(&self, id: &str) -> Option<Problem> {
        self.find(id).is_none().then(|| Problem {
            path: "theme".into(),
            message: format!(
                "unknown theme \"{id}\"; themes: {}",
                self.ids().collect::<Vec<_>>().join(", ")
            ),
        })
    }

    /// Theme `id` drawn at `depth` (`Auto` means 24-bit); an unknown one is Üsküdar. A
    /// painting theme needs 256 colours at least; with 16 the terminal theme stands in.
    pub fn get(&self, id: &str, depth: ColorDepth) -> Theme {
        let source = self
            .find(id)
            .or_else(|| self.find("uskudar"))
            .expect("Üsküdar is built in");
        if depth == ColorDepth::Ansi16 && source.spec.paints() {
            // The built-in one: a user's terminal.toml might paint.
            let terminal = Spec::parse(TERMINAL).expect("the terminal theme is valid");
            let mut theme = terminal.resolve("terminal", depth);
            theme.stands_in_for = Some(source.id.clone());
            return theme;
        }
        source.spec.resolve(&source.id, depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> Spec {
        let source = BUILTIN.iter().find(|(i, _)| *i == id).unwrap().1;
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
        for (id, _) in BUILTIN {
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
        assert_eq!(t.stands_in_for.as_deref(), Some("moda"));
        assert_eq!(
            Theme::named("terminal", ColorDepth::Ansi16).stands_in_for,
            None
        );
    }

    fn user_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn a_user_theme_is_loaded_and_fills_in_from_uskudar() {
        let dir = user_dir(&[(
            "deniz.toml",
            "name = \"Deniz\"\n[ui]\naccent = \"#ff0000\"\n",
        )]);
        let (themes, problems) = Themes::load(dir.path());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(themes.ids().last(), Some("deniz"));
        let t = themes.get("deniz", ColorDepth::TrueColor);
        assert_eq!(t.id, "deniz");
        assert_eq!(t.name, "Deniz");
        assert_eq!(t.accent.fg, Some(Color::Rgb(0xff, 0, 0)));
        assert_eq!(
            t.base.bg,
            Some(Color::Rgb(0x0f, 0x1d, 0x2e)),
            "Üsküdar's ground"
        );
    }

    #[test]
    fn a_user_theme_without_a_name_is_named_after_its_file() {
        let dir = user_dir(&[("gece.toml", "[ui]\nfg = \"#ffffff\"\n")]);
        let (themes, _) = Themes::load(dir.path());
        assert_eq!(themes.name_of("gece"), "gece");
    }

    #[test]
    fn a_user_file_takes_the_place_of_a_builtin() {
        let dir = user_dir(&[("moda.toml", "name = \"My Moda\"\n")]);
        let (themes, _) = Themes::load(dir.path());
        assert_eq!(themes.name_of("moda"), "My Moda");
        let builtin: Vec<String> = Themes::builtin().ids().map(String::from).collect();
        let loaded: Vec<String> = themes.ids().map(String::from).collect();
        assert_eq!(loaded, builtin, "same place, no new entry");
    }

    #[test]
    fn a_broken_user_theme_is_skipped_and_reported() {
        let dir = user_dir(&[
            ("bad.toml", "not = [valid"),
            ("red.toml", "[ui]\nfg = \"red-ish\"\n"),
            ("notes.txt", "not a theme"),
        ]);
        let (themes, problems) = Themes::load(dir.path());
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|p| p.message.starts_with("theme not used"))
        );
        assert!(problems.iter().any(|p| p.path.ends_with("bad.toml")));
        assert!(
            !themes
                .ids()
                .any(|id| id == "bad" || id == "red" || id == "notes")
        );
    }

    #[test]
    fn no_themes_folder_is_fine() {
        let (themes, problems) = Themes::load(Path::new("/no/such/folder"));
        assert!(problems.is_empty());
        assert_eq!(themes, Themes::builtin());
    }

    #[test]
    fn an_unknown_theme_falls_back_and_is_reported() {
        let themes = Themes::builtin();
        assert_eq!(themes.get("silinmis", ColorDepth::TrueColor).id, "uskudar");
        let problem = themes.check("silinmis").unwrap();
        assert_eq!(problem.path, "theme");
        assert!(problem.message.starts_with("unknown theme \"silinmis\""));
        assert_eq!(themes.check("moda"), None);
    }

    #[test]
    fn a_broken_theme_file_says_what_is_wrong() {
        let src = USKUDAR.replace("dim = \"#93a4b8\"", "dim = \"#12\"");
        let err = Spec::parse(&src).unwrap_err();
        assert!(err.contains("ui.dim"), "{err}");
        let err = Spec::parse("name = \"x\"\n").unwrap_err();
        assert!(err.contains("status.fresh"), "{err}");
        // A Windows checkout may have CRLF line ends.
        let src = USKUDAR
            .replace("\r\n", "\n")
            .replace("\"#ffffff\",\n]", "]");
        let err = Spec::parse(&src).unwrap_err();
        assert!(err.contains("16 colours"), "{err}");
    }
}
