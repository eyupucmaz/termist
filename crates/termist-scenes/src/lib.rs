//! The Istanbul scenes: hand-drawn landmarks coloured by the time of day, with a few
//! gentle animations.
//!
//! This is the Rust twin of `tools/scene-preview.py`, where the scenes were drawn and
//! approved: the same files (`assets/scenes/`, built in), the same palette merging and
//! the same eight effects with the same formulas. Only the random placements (stars,
//! which windows go dark) come from a different generator than Python's.
use std::collections::HashMap;
use toml::{Table, Value};

pub type Rgb = (u8, u8, u8);

/// One character cell of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeOfDay {
    /// 06-11
    Sabah,
    /// 11-17
    Gunduz,
    /// 17-20, sunset
    Aksam,
    /// 20-06
    Gece,
}

impl TimeOfDay {
    pub const ALL: [TimeOfDay; 4] = [
        TimeOfDay::Sabah,
        TimeOfDay::Gunduz,
        TimeOfDay::Aksam,
        TimeOfDay::Gece,
    ];

    pub fn from_hour(hour: u32) -> TimeOfDay {
        match hour {
            6..=10 => TimeOfDay::Sabah,
            11..=16 => TimeOfDay::Gunduz,
            17..=19 => TimeOfDay::Aksam,
            _ => TimeOfDay::Gece,
        }
    }

    /// The table name in the palette files.
    pub fn key(self) -> &'static str {
        match self {
            TimeOfDay::Sabah => "sabah",
            TimeOfDay::Gunduz => "gunduz",
            TimeOfDay::Aksam => "aksam",
            TimeOfDay::Gece => "gece",
        }
    }
}

/// Every scene, in the order of the docs: `(name, art, mask, scene.toml, palette.toml)`.
const SOURCES: [(&str, &str, &str, &str, Option<&str>); 6] = [
    (
        "galata",
        include_str!("../../../assets/scenes/galata/large.art.txt"),
        include_str!("../../../assets/scenes/galata/large.mask.txt"),
        include_str!("../../../assets/scenes/galata/scene.toml"),
        None,
    ),
    (
        "kiz-kulesi",
        include_str!("../../../assets/scenes/kiz-kulesi/large.art.txt"),
        include_str!("../../../assets/scenes/kiz-kulesi/large.mask.txt"),
        include_str!("../../../assets/scenes/kiz-kulesi/scene.toml"),
        Some(include_str!(
            "../../../assets/scenes/kiz-kulesi/palette.toml"
        )),
    ),
    (
        "ayasofya",
        include_str!("../../../assets/scenes/ayasofya/large.art.txt"),
        include_str!("../../../assets/scenes/ayasofya/large.mask.txt"),
        include_str!("../../../assets/scenes/ayasofya/scene.toml"),
        Some(include_str!("../../../assets/scenes/ayasofya/palette.toml")),
    ),
    (
        "kopru",
        include_str!("../../../assets/scenes/kopru/large.art.txt"),
        include_str!("../../../assets/scenes/kopru/large.mask.txt"),
        include_str!("../../../assets/scenes/kopru/scene.toml"),
        Some(include_str!("../../../assets/scenes/kopru/palette.toml")),
    ),
    (
        "vapur",
        include_str!("../../../assets/scenes/vapur/large.art.txt"),
        include_str!("../../../assets/scenes/vapur/large.mask.txt"),
        include_str!("../../../assets/scenes/vapur/scene.toml"),
        Some(include_str!("../../../assets/scenes/vapur/palette.toml")),
    ),
    (
        "yerebatan",
        include_str!("../../../assets/scenes/yerebatan/large.art.txt"),
        include_str!("../../../assets/scenes/yerebatan/large.mask.txt"),
        include_str!("../../../assets/scenes/yerebatan/scene.toml"),
        Some(include_str!(
            "../../../assets/scenes/yerebatan/palette.toml"
        )),
    ),
];

const SHARED_PALETTES: &str = include_str!("../../../assets/scenes/palettes.toml");

/// The names of the built-in scenes.
pub fn names() -> impl Iterator<Item = &'static str> {
    SOURCES.iter().map(|s| s.0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KeyColor {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
}

/// One time of day's colours: the shared palette with the scene's own over it.
#[derive(Clone, Debug, Default)]
struct Palette {
    sky: Vec<Rgb>,
    water: Option<Vec<Rgb>>,
    wave: Option<Rgb>,
    keys: HashMap<char, KeyColor>,
}

impl Palette {
    fn key(&self, k: char) -> Option<KeyColor> {
        self.keys.get(&k).copied()
    }

    /// The `D` key's colour, the default of a light that goes dark.
    fn dark(&self) -> Rgb {
        self.key('D')
            .and_then(|c| c.fg)
            .unwrap_or((0x33, 0x33, 0x33))
    }
}

#[derive(Clone, Debug)]
enum Effect {
    Waves {
        key: char,
        every: i64,
        glints: bool,
        glint_density: f64,
        glint_rows: f64,
    },
    Stars {
        count: usize,
    },
    Gulls {
        rows: Vec<i64>,
        color: Rgb,
    },
    Smoke {
        key: char,
        drift: i64,
    },
    Twinkle {
        key: char,
        threshold: f64,
        off: Option<Rgb>,
    },
    Blink {
        key: char,
        colors: Vec<Rgb>,
        period: i64,
        wave: i64,
    },
    Sprite(Box<Sprite>),
    Drip {
        col: usize,
        top: usize,
        bottom: usize,
        period: i64,
        phase: i64,
        over: String,
        color: Rgb,
    },
}

/// Rows of characters: art or mask.
type Grid = Vec<Vec<char>>;
/// A cell and its fixed random number.
type Seed = ((usize, usize), f64);

#[derive(Clone, Debug)]
struct Sprite {
    /// Frames of art and of mask (one each without flip-book animation), padded.
    frames: Vec<(Grid, Grid)>,
    frame_every: i64,
    row: i64,
    speed: f64,
    direction: i64,
    over: String,
    smoke_key: Option<char>,
    twinkle_key: Option<char>,
    twinkle_times: Vec<String>,
    twinkle_off: Option<Rgb>,
}

#[derive(Clone, Debug)]
struct Placed {
    effect: Effect,
    /// The times of day it runs at; `None` for always.
    times: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Scene {
    pub name: &'static str,
    pub title: String,
    pub width: usize,
    pub height: usize,
    art: Vec<Vec<char>>,
    mask: Vec<Vec<char>>,
    effects: Vec<Placed>,
    palettes: HashMap<TimeOfDay, Palette>,
    horizon: usize,
    horizon_is_water: bool,
    /// Per twinkling key, a fixed random number for each of its cells.
    seeds: HashMap<char, Vec<Seed>>,
    /// Per star count: where the stars are, their glyph and their phase.
    stars: HashMap<usize, Vec<(usize, usize, char, f64)>>,
}

fn hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// Python's `round`: halves to even.
fn round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// A colour between gradient stops at `t` (0 top, 1 bottom).
fn gradient(stops: &[Rgb], t: f64) -> Rgb {
    match stops {
        [] => (0, 0, 0),
        [one] => *one,
        _ => {
            let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
            let i = (t as usize).min(stops.len() - 2);
            let f = t - i as f64;
            let (a, b) = (stops[i], stops[i + 1]);
            let mix = |x: u8, y: u8| round(x as f64 + (y as f64 - x as f64) * f) as u8;
            (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
        }
    }
}

/// A small fixed-seed generator (SplitMix64): the same placements on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn stable_seed(text: &str) -> u64 {
    text.chars()
        .enumerate()
        .map(|(i, c)| (i as u64 + 1) * c as u64)
        .sum()
}

fn lines(text: &str) -> Vec<Vec<char>> {
    text.trim_end_matches('\n')
        .split('\n')
        .map(|l| l.chars().collect())
        .collect()
}

fn pad(rows: &mut [Vec<char>], width: usize) {
    for r in rows {
        r.resize(width, ' ');
    }
}

fn read_palettes(text: &str) -> Result<HashMap<String, Table>, String> {
    let t: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    Ok(t.into_iter()
        .filter_map(|(k, v)| match v {
            Value::Table(t) => Some((k, t)),
            _ => None,
        })
        .collect())
}

fn colours(v: Option<&Value>) -> Option<Vec<Rgb>> {
    v?.as_array()?
        .iter()
        .map(|c| c.as_str().and_then(hex))
        .collect()
}

fn palette(table: &Table) -> Result<Palette, String> {
    let mut p = Palette {
        sky: colours(table.get("sky")).unwrap_or_default(),
        water: colours(table.get("water")),
        wave: table.get("wave").and_then(Value::as_str).and_then(hex),
        keys: HashMap::new(),
    };
    for (k, v) in table {
        let mut chars = k.chars();
        let (Some(key), None) = (chars.next(), chars.next()) else {
            continue;
        };
        let t = v
            .as_table()
            .ok_or(format!("palette key {k} should be a table"))?;
        let get = |name: &str| -> Result<Option<Rgb>, String> {
            t.get(name)
                .map(|c| {
                    c.as_str()
                        .and_then(hex)
                        .ok_or(format!("{k}.{name} is not #rrggbb"))
                })
                .transpose()
        };
        p.keys.insert(
            key,
            KeyColor {
                fg: get("fg")?,
                bg: get("bg")?,
            },
        );
    }
    Ok(p)
}

fn one_char(t: &Table, name: &str, default: char) -> char {
    t.get(name)
        .and_then(Value::as_str)
        .and_then(|s| s.chars().next())
        .unwrap_or(default)
}

fn int(t: &Table, name: &str, default: i64) -> i64 {
    t.get(name).and_then(Value::as_integer).unwrap_or(default)
}

fn float(t: &Table, name: &str, default: f64) -> f64 {
    match t.get(name) {
        Some(Value::Float(f)) => *f,
        Some(Value::Integer(i)) => *i as f64,
        _ => default,
    }
}

fn text(t: &Table, name: &str, default: &str) -> String {
    t.get(name)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

fn strings(t: &Table, name: &str) -> Option<Vec<String>> {
    t.get(name)?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn art_rows(v: &Value) -> Option<Vec<Vec<char>>> {
    v.as_array()?
        .iter()
        .map(|r| r.as_str().map(|s| s.chars().collect()))
        .collect()
}

fn effect(t: &Table) -> Result<Effect, String> {
    let kind = text(t, "type", "");
    Ok(match kind.as_str() {
        "waves" => Effect::Waves {
            key: one_char(t, "key", 'w'),
            every: int(t, "every", 2).max(1),
            glints: t.get("glints").and_then(Value::as_bool).unwrap_or(true),
            glint_density: float(t, "glint_density", 1.0),
            glint_rows: float(t, "glint_rows", 4.0),
        },
        "stars" => Effect::Stars {
            count: int(t, "count", 28).max(0) as usize,
        },
        "gulls" => Effect::Gulls {
            rows: t
                .get("rows")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_integer).collect())
                .unwrap_or_else(|| vec![4, 6, 3]),
            color: t
                .get("color")
                .and_then(Value::as_str)
                .and_then(hex)
                .unwrap_or((0xff, 0xfa, 0xf5)),
        },
        "smoke" => Effect::Smoke {
            key: one_char(t, "key", 'F'),
            drift: int(t, "drift", -1),
        },
        "twinkle" => Effect::Twinkle {
            key: one_char(t, "key", 'W'),
            threshold: float(t, "threshold", 0.85),
            off: t.get("off").and_then(Value::as_str).and_then(hex),
        },
        "blink" => Effect::Blink {
            key: one_char(t, "key", ' '),
            colors: colours(t.get("colors")).ok_or("blink needs colors")?,
            period: int(t, "period", 20).max(1),
            wave: int(t, "wave", 0),
        },
        "sprite" => {
            let frames: Vec<Vec<Vec<char>>> = match t.get("frames") {
                Some(Value::Array(frames)) => frames
                    .iter()
                    .map(art_rows)
                    .collect::<Option<_>>()
                    .ok_or("sprite frames should be lists of text")?,
                _ => vec![art_rows(t.get("art").ok_or("sprite needs art")?).ok_or("bad art")?],
            };
            let masks: Vec<Vec<Vec<char>>> = match t.get("mask_frames") {
                Some(Value::Array(masks)) => masks
                    .iter()
                    .map(art_rows)
                    .collect::<Option<_>>()
                    .ok_or("sprite mask_frames should be lists of text")?,
                _ => vec![art_rows(t.get("mask").ok_or("sprite needs mask")?).ok_or("bad mask")?],
            };
            let frames = frames
                .into_iter()
                .enumerate()
                .map(|(i, mut art)| {
                    let mut mask = masks[i.min(masks.len() - 1)].clone();
                    let w = art.iter().chain(&mask).map(Vec::len).max().unwrap_or(0);
                    pad(&mut art, w);
                    pad(&mut mask, w);
                    (art, mask)
                })
                .collect();
            Effect::Sprite(Box::new(Sprite {
                frames,
                frame_every: int(t, "frame_every", 3).max(1),
                row: int(t, "row", 0),
                speed: float(t, "speed", 0.3),
                direction: int(t, "direction", 1),
                over: text(t, "over", " w"),
                smoke_key: t
                    .get("smoke_key")
                    .and_then(Value::as_str)
                    .and_then(|s| s.chars().next()),
                twinkle_key: t
                    .get("twinkle_key")
                    .and_then(Value::as_str)
                    .and_then(|s| s.chars().next()),
                twinkle_times: strings(t, "twinkle_times")
                    .unwrap_or_else(|| vec!["aksam".into(), "gece".into()]),
                twinkle_off: t.get("twinkle_off").and_then(Value::as_str).and_then(hex),
            }))
        }
        "drip" => Effect::Drip {
            col: int(t, "col", 0).max(0) as usize,
            top: int(t, "top", 0).max(0) as usize,
            bottom: int(t, "bottom", 0).max(0) as usize,
            period: int(t, "period", 40).max(1),
            phase: int(t, "phase", 0),
            over: text(t, "over", " "),
            color: t
                .get("color")
                .and_then(Value::as_str)
                .and_then(hex)
                .unwrap_or((0xbf, 0xe6, 0xff)),
        },
        other => return Err(format!("unknown effect type {other:?}")),
    })
}

impl Scene {
    /// The built-in scene `name`.
    pub fn load(name: &str) -> Result<Scene, String> {
        let (name, art, mask, meta, own) = *SOURCES
            .iter()
            .find(|s| s.0 == name)
            .ok_or(format!("no scene {name:?}"))?;
        let (mut art, mut mask) = (lines(art), lines(mask));
        if art.len() != mask.len() {
            return Err(format!(
                "{name}: art has {} rows, mask has {}",
                art.len(),
                mask.len()
            ));
        }
        let width = art.iter().chain(&mask).map(Vec::len).max().unwrap_or(0);
        pad(&mut art, width);
        pad(&mut mask, width);
        let height = art.len();
        let meta: Table = meta
            .parse()
            .map_err(|e: toml::de::Error| format!("{name}: {e}"))?;
        let shared = read_palettes(SHARED_PALETTES)?;
        let own = own.map(read_palettes).transpose()?.unwrap_or_default();
        let mut palettes = HashMap::new();
        for tod in TimeOfDay::ALL {
            let mut merged = shared.get(tod.key()).cloned().unwrap_or_default();
            if let Some(o) = own.get(tod.key()) {
                for (k, v) in o {
                    merged.insert(k.clone(), v.clone());
                }
            }
            palettes.insert(tod, palette(&merged).map_err(|e| format!("{name}: {e}"))?);
        }
        let mut effects = Vec::new();
        for e in meta
            .get("effects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let t = e
                .as_table()
                .ok_or(format!("{name}: an effect is not a table"))?;
            effects.push(Placed {
                effect: effect(t).map_err(|e| format!("{name}: {e}"))?,
                times: strings(t, "times"),
            });
        }
        let horizon_key = one_char(&meta, "horizon_key", 'w');
        let horizon = mask
            .iter()
            .position(|r| r.contains(&horizon_key))
            .unwrap_or(height);
        let horizon_is_water = effects
            .iter()
            .any(|p| matches!(p.effect, Effect::Waves { key, .. } if key == horizon_key));
        let mut scene = Scene {
            name,
            title: text(&meta, "title", name),
            width,
            height,
            art,
            mask,
            effects,
            palettes,
            horizon,
            horizon_is_water,
            seeds: HashMap::new(),
            stars: HashMap::new(),
        };
        scene.place();
        Ok(scene)
    }

    /// The fixed random placements its effects need.
    fn place(&mut self) {
        for p in self.effects.clone() {
            match p.effect {
                Effect::Twinkle { key, .. } if !self.seeds.contains_key(&key) => {
                    let mut rng = Rng(stable_seed(&key.to_string()));
                    let mut cells = Vec::new();
                    for y in 0..self.height {
                        for x in 0..self.width {
                            if self.mask[y][x] == key {
                                cells.push(((y, x), rng.unit()));
                            }
                        }
                    }
                    self.seeds.insert(key, cells);
                }
                Effect::Stars { count } if !self.stars.contains_key(&count) => {
                    let mut rng = Rng(42);
                    let mut sky: Vec<(usize, usize)> = (0..self.horizon.saturating_sub(6))
                        .flat_map(|y| (0..self.width).map(move |x| (y, x)))
                        .filter(|&(y, x)| self.mask[y][x] == ' ')
                        .collect();
                    let glyphs = ['.', '·', '*', '+'];
                    let mut stars = Vec::new();
                    for _ in 0..count.min(sky.len()) {
                        let (y, x) = sky.swap_remove(rng.below(sky.len()));
                        // 6.28, not TAU: the phases of the approved previewer.
                        #[allow(clippy::approx_constant)]
                        let phase = rng.unit() * 6.28;
                        stars.push((y, x, glyphs[rng.below(4)], phase));
                    }
                    self.stars.insert(count, stars);
                }
                _ => {}
            }
        }
    }

    /// Frame `n` (10 a second) at `tod`.
    pub fn frame(&self, tod: TimeOfDay, n: u64) -> Vec<Vec<Cell>> {
        let n = n as i64;
        let pal = &self.palettes[&tod];
        let below = self.height.saturating_sub(self.horizon);
        let mut cells = Vec::with_capacity(self.height);
        for y in 0..self.height {
            let sky = match &pal.water {
                Some(water) if y >= self.horizon && self.horizon_is_water => gradient(
                    water,
                    (y - self.horizon) as f64 / below.saturating_sub(1).max(1) as f64,
                ),
                _ => gradient(
                    &pal.sky,
                    y as f64 / self.horizon.saturating_sub(1).max(1) as f64,
                ),
            };
            let row = (0..self.width)
                .map(|x| {
                    let (key, ch) = (self.mask[y][x], self.art[y][x]);
                    match pal.key(key).filter(|_| key != ' ') {
                        Some(spec) => Cell {
                            ch,
                            fg: spec.fg.unwrap_or((255, 255, 255)),
                            bg: spec.bg.unwrap_or(sky),
                        },
                        None => Cell {
                            ch,
                            fg: (255, 255, 255),
                            bg: sky,
                        },
                    }
                })
                .collect();
            cells.push(row);
        }
        for placed in &self.effects {
            let on = placed
                .times
                .as_ref()
                .is_none_or(|t| t.iter().any(|t| t == tod.key()));
            if on {
                self.apply(&placed.effect, &mut cells, pal, tod, n);
            }
        }
        cells
    }

    fn apply(
        &self,
        effect: &Effect,
        cells: &mut [Vec<Cell>],
        pal: &Palette,
        tod: TimeOfDay,
        n: i64,
    ) {
        let (w, h) = (self.width as i64, self.height as i64);
        match effect {
            Effect::Waves {
                key,
                every,
                glints,
                glint_density,
                glint_rows,
            } => {
                let rows: Vec<usize> = (0..self.height)
                    .filter(|&y| self.mask[y].contains(key))
                    .collect();
                let (Some(&top), Some(&bottom)) = (rows.first(), rows.last()) else {
                    return;
                };
                let scale = (glint_rows / rows.len() as f64).min(1.0) * glint_density;
                let star_t = (0.993f64.acos() * scale).cos();
                let dot_t = (0.975f64.acos() * scale).cos();
                let water = pal.water.clone().unwrap_or_else(|| pal.sky.clone());
                let wave = pal.wave.unwrap_or((255, 255, 255));
                for &y in &rows {
                    let pattern: Vec<char> = (0..self.width)
                        .map(|x| {
                            if self.mask[y][x] == *key {
                                self.art[y][x]
                            } else {
                                ' '
                            }
                        })
                        .collect();
                    let offset = n.div_euclid(*every) * if y % 2 == 1 { 1 } else { -1 };
                    let bg = gradient(&water, (y - top) as f64 / (bottom - top).max(1) as f64);
                    for x in 0..self.width {
                        if self.mask[y][x] != *key {
                            continue;
                        }
                        let mut ch = pattern[(x as i64 + offset).rem_euclid(w) as usize];
                        let mut fg = wave;
                        if ch != '~' {
                            ch = ' ';
                            if *glints {
                                let g = (x as f64 * 12.9898 + y as f64 * 78.233 + n as f64 * 0.35)
                                    .sin();
                                if g > star_t {
                                    (ch, fg) = ('*', (255, 255, 240));
                                } else if g > dot_t {
                                    ch = '.';
                                }
                            }
                        }
                        cells[y][x] = Cell { ch, fg, bg };
                    }
                }
            }
            Effect::Stars { count } => {
                for &(y, x, ch, ph) in self.stars.get(count).into_iter().flatten() {
                    if (n as f64 / 7.0 + ph).sin() > -0.3 {
                        cells[y][x].ch = ch;
                        cells[y][x].fg = (220, 225, 245);
                    }
                }
            }
            Effect::Gulls { rows, color } => {
                const SPEEDS: [f64; 4] = [1.1, 0.8, 0.65, 0.95];
                for (i, row) in rows.iter().enumerate() {
                    let (speed, phase) = (SPEEDS[i % 4], i as i64 * 30);
                    let x =
                        w - 1 - ((n as f64 * speed + (phase * 3) as f64) % (w + 6) as f64) as i64;
                    let y = row + round(((n + phase) as f64 / 9.0).sin());
                    let sprite = if (n.div_euclid(3) + i as i64) % 2 == 1 {
                        "\\v/"
                    } else {
                        "-v-"
                    };
                    for (k, ch) in sprite.chars().enumerate() {
                        let cx = x + k as i64;
                        if (0..w).contains(&cx)
                            && (0..h).contains(&y)
                            && self.mask[y as usize][cx as usize] == ' '
                        {
                            let c = &mut cells[y as usize][cx as usize];
                            c.ch = ch;
                            c.fg = *color;
                        }
                    }
                }
            }
            Effect::Smoke { key, drift } => {
                let src: Vec<(usize, usize)> = (0..self.height)
                    .flat_map(|y| (0..self.width).map(move |x| (y, x)))
                    .filter(|&(y, x)| self.mask[y][x] == *key)
                    .collect();
                if let Some(&(top, _)) = src.iter().min() {
                    let cx = src.iter().map(|&(_, x)| x).sum::<usize>() / src.len();
                    self.puffs(cells, tod, n, top as i64, cx as i64, *drift);
                }
            }
            Effect::Twinkle {
                key,
                threshold,
                off,
            } => {
                let off = off.unwrap_or_else(|| pal.dark());
                for &((y, x), seed) in self.seeds.get(key).into_iter().flatten() {
                    if (n as f64 / 40.0 + seed * 50.0).sin() > *threshold {
                        cells[y][x].fg = off;
                    }
                }
            }
            Effect::Blink {
                key,
                colors,
                period,
                wave,
            } => {
                for (row, mask) in cells.iter_mut().zip(&self.mask) {
                    for (x, (cell, k)) in row.iter_mut().zip(mask).enumerate() {
                        if k == key {
                            let i = (n + x as i64 * wave).div_euclid(*period);
                            cell.fg = colors[i.rem_euclid(colors.len() as i64) as usize];
                        }
                    }
                }
            }
            Effect::Sprite(s) => self.sprite(s, cells, pal, tod, n),
            Effect::Drip {
                col,
                top,
                bottom,
                period,
                phase,
                over,
                color,
            } => {
                let t = (n + phase).rem_euclid(*period);
                let fall = *bottom as i64 - *top as i64;
                if t < fall {
                    let y = top + t as usize;
                    if y < self.height && *col < self.width && over.contains(self.mask[y][*col]) {
                        cells[y][*col].ch = '\'';
                        cells[y][*col].fg = *color;
                    }
                } else {
                    let r = t - fall;
                    if r < 6 && *bottom < self.height {
                        let mut xs = vec![-r * 2, r * 2];
                        xs.dedup();
                        for dx in xs {
                            let x = *col as i64 + dx;
                            if (0..w).contains(&x) {
                                let c = &mut cells[*bottom][x as usize];
                                c.ch = match (r, dx < 0) {
                                    (0, _) => 'o',
                                    (_, true) => '(',
                                    _ => ')',
                                };
                                c.fg = *color;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Five staggered puffs rising from (cy, cx), drifting sideways, over sky cells.
    fn puffs(&self, cells: &mut [Vec<Cell>], tod: TimeOfDay, n: i64, cy: i64, cx: i64, drift: i64) {
        for k in 0..5 {
            let age = (n + k * 8).rem_euclid(40);
            let (py, px) = (cy - 1 - age / 8, cx + drift * (age / 5));
            let ch = if age < 10 {
                'o'
            } else if age < 24 {
                'O'
            } else {
                '.'
            };
            if (0..self.height as i64).contains(&py)
                && (0..self.width as i64).contains(&px)
                && self.mask[py as usize][px as usize] == ' '
            {
                let shade = if tod != TimeOfDay::Gece {
                    235 - age * 3
                } else {
                    120 - age
                } as u8;
                let c = &mut cells[py as usize][px as usize];
                c.ch = ch;
                c.fg = (shade, shade, shade);
            }
        }
    }

    fn sprite(&self, s: &Sprite, cells: &mut [Vec<Cell>], pal: &Palette, tod: TimeOfDay, n: i64) {
        let i = n
            .div_euclid(s.frame_every)
            .rem_euclid(s.frames.len() as i64) as usize;
        let (art, mask) = &s.frames[i];
        let sw = art.first().map_or(0, Vec::len) as i64;
        let w = self.width as i64;
        let pos = ((n as f64 * s.speed) as i64).rem_euclid(w + sw);
        let x0 = if s.direction > 0 { pos - sw } else { w - pos };
        if let Some(smoke) = s.smoke_key {
            let src: Vec<(i64, i64)> = mask
                .iter()
                .enumerate()
                .flat_map(|(dy, r)| {
                    r.iter()
                        .enumerate()
                        .filter(move |(_, k)| **k == smoke)
                        .map(move |(dx, _)| (s.row + dy as i64, x0 + dx as i64))
                })
                .collect();
            if let Some(&(top, _)) = src.iter().min() {
                let cx = src
                    .iter()
                    .map(|&(_, x)| x)
                    .sum::<i64>()
                    .div_euclid(src.len() as i64);
                let drift = if s.direction > 0 { -1 } else { 1 };
                self.puffs(cells, tod, n, top, cx, drift);
            }
        }
        let twinkles = s.twinkle_times.iter().any(|t| t == tod.key());
        for (dy, (ar, mr)) in art.iter().zip(mask).enumerate() {
            let y = s.row + dy as i64;
            for (dx, (&ch, &k)) in ar.iter().zip(mr).enumerate() {
                let x = x0 + dx as i64;
                if k == '.'
                    || !(0..w).contains(&x)
                    || !(0..self.height as i64).contains(&y)
                    || !s.over.contains(self.mask[y as usize][x as usize])
                {
                    continue;
                }
                let cell = &mut cells[y as usize][x as usize];
                let spec = pal.key(k).unwrap_or_default();
                let mut fg = spec.fg.unwrap_or(cell.fg);
                let bg = spec.bg.unwrap_or(cell.bg);
                if Some(k) == s.twinkle_key && twinkles {
                    let seed = ((dx * 7919 + dy * 104_729) % 1000) as f64 / 20.0;
                    if (n as f64 / 40.0 + seed).sin() > 0.85 {
                        fg = s.twinkle_off.unwrap_or_else(|| pal.dark());
                    }
                }
                *cell = Cell { ch, fg, bg };
            }
        }
    }

    /// What is wrong with the scene's files: mask keys with no colour, palettes without
    /// a sky (or water, when waves need it). Also draws a few frames of each palette.
    pub fn check(&self) -> Vec<String> {
        let mut keys: Vec<char> = self.mask.iter().flatten().copied().collect();
        let mut water = Vec::new();
        for p in &self.effects {
            match &p.effect {
                Effect::Sprite(s) => {
                    for (_, mask) in &s.frames {
                        keys.extend(mask.iter().flatten().filter(|k| **k != '.'));
                    }
                }
                Effect::Waves { key, .. } => water.push(*key),
                _ => {}
            }
        }
        keys.sort_unstable();
        keys.dedup();
        keys.retain(|k| *k != ' ' && !water.contains(k));
        let mut problems = Vec::new();
        for tod in TimeOfDay::ALL {
            let pal = &self.palettes[&tod];
            let missing: String = keys.iter().filter(|k| pal.key(**k).is_none()).collect();
            if !missing.is_empty() {
                problems.push(format!(
                    "{}: mask keys without a colour: {missing}",
                    tod.key()
                ));
            }
            if pal.sky.is_empty() {
                problems.push(format!("{}: palette has no sky", tod.key()));
            }
            if !water.is_empty() && (pal.water.is_none() || pal.wave.is_none()) {
                problems.push(format!("{}: palette has no water or wave", tod.key()));
            }
            for n in [0, 7, 55, 311] {
                self.frame(tod, n);
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_loads_and_every_key_has_a_colour() {
        for name in names() {
            let scene = Scene::load(name).unwrap();
            assert_eq!((scene.width, scene.height), (96, 26), "{name}");
            let problems = scene.check();
            assert!(problems.is_empty(), "{name}: {problems:#?}");
        }
    }

    #[test]
    fn a_frame_is_the_scene_size_and_moves() {
        let scene = Scene::load("galata").unwrap();
        let first = scene.frame(TimeOfDay::Gunduz, 0);
        assert_eq!(first.len(), 26);
        assert!(first.iter().all(|r| r.len() == 96));
        assert_ne!(
            first,
            scene.frame(TimeOfDay::Gunduz, 5),
            "the water and gulls move"
        );
        assert_eq!(
            first,
            scene.frame(TimeOfDay::Gunduz, 0),
            "the same frame twice"
        );
    }

    #[test]
    fn the_sky_follows_its_gradient_and_the_time_of_day() {
        let scene = Scene::load("galata").unwrap();
        let day = scene.frame(TimeOfDay::Gunduz, 0);
        assert_eq!(day[0][0].bg, (0x3f, 0x97, 0xdb), "top of the day sky");
        let night = scene.frame(TimeOfDay::Gece, 0);
        assert_eq!(night[0][0].bg, (0x05, 0x08, 0x16), "top of the night sky");
    }

    #[test]
    fn the_hours_pick_the_palette() {
        assert_eq!(TimeOfDay::from_hour(6), TimeOfDay::Sabah);
        assert_eq!(TimeOfDay::from_hour(11), TimeOfDay::Gunduz);
        assert_eq!(TimeOfDay::from_hour(17), TimeOfDay::Aksam);
        assert_eq!(TimeOfDay::from_hour(20), TimeOfDay::Gece);
        assert_eq!(TimeOfDay::from_hour(3), TimeOfDay::Gece);
    }

    #[test]
    fn gradients_mix_like_the_previewer() {
        let stops = [(0, 0, 0), (100, 200, 250)];
        assert_eq!(gradient(&stops, 0.0), (0, 0, 0));
        assert_eq!(gradient(&stops, 1.0), (100, 200, 250));
        assert_eq!(gradient(&stops, 0.5), (50, 100, 125));
        assert_eq!(gradient(&[(1, 2, 3)], 0.7), (1, 2, 3));
        assert_eq!(round(2.5), 2, "halves to even, as Python");
    }

    #[test]
    fn a_vapur_crosses_and_a_drop_falls() {
        let vapur = Scene::load("vapur").unwrap();
        let a = vapur.frame(TimeOfDay::Gunduz, 100);
        let b = vapur.frame(TimeOfDay::Gunduz, 140);
        let hull_cols = |f: &Vec<Vec<Cell>>| (0..96).filter(|&x| f[17][x].ch == '_').count();
        assert!(
            hull_cols(&a) > 0 || hull_cols(&b) > 0,
            "the ferry is in view at some point"
        );
        let cistern = Scene::load("yerebatan").unwrap();
        let frames: Vec<_> = (0..70).map(|n| cistern.frame(TimeOfDay::Gece, n)).collect();
        assert!(
            frames.iter().any(|f| (1..24).any(|y| f[y][58].ch == '\'')),
            "a drop falls down column 58"
        );
    }
}
