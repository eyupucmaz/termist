//! The user's settings: `config.toml`, with `config.local.toml` layered over it.
//!
//! A config never stops termist from starting: an unknown key or a bad value is a
//! [`Problem`], and that one setting keeps its default.
use crate::model::Harness;
use std::collections::BTreeMap;
use toml::{Table, Value};

/// The built-in themes of this version, in the order the settings screen offers them.
pub const THEMES: [&str; 3] = ["uskudar", "moda", "terminal"];
/// Themes planned for a later version: named in the docs, not drawn yet.
const LATER_THEMES: [&str; 4] = ["aksaray", "kadikoy", "besiktas", "balat"];
pub const SCENES: [&str; 6] = [
    "galata",
    "kiz-kulesi",
    "ayasofya",
    "kopru",
    "vapur",
    "yerebatan",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanePosition {
    #[default]
    Auto,
    Bottom,
    Right,
}

impl PanePosition {
    pub const ALL: [PanePosition; 3] = [
        PanePosition::Auto,
        PanePosition::Bottom,
        PanePosition::Right,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PanePosition::Auto => "auto",
            PanePosition::Bottom => "bottom",
            PanePosition::Right => "right",
        }
    }
}

/// How many colours the host terminal draws; `Auto` asks the environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorDepth {
    #[default]
    Auto,
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorDepth {
    pub const ALL: [ColorDepth; 4] = [
        ColorDepth::Auto,
        ColorDepth::TrueColor,
        ColorDepth::Ansi256,
        ColorDepth::Ansi16,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ColorDepth::Auto => "auto",
            ColorDepth::TrueColor => "truecolor",
            ColorDepth::Ansi256 => "256",
            ColorDepth::Ansi16 => "16",
        }
    }
}

/// The sound for one kind of alert: one of termist's recordings, the system's, the
/// terminal bell, or none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sound {
    /// A seagull.
    Marti,
    /// A cat's meow.
    Kedi,
    System,
    Bell,
    #[default]
    Off,
}

impl Sound {
    pub const ALL: [Sound; 5] = [
        Sound::Marti,
        Sound::Kedi,
        Sound::System,
        Sound::Bell,
        Sound::Off,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Sound::Marti => "marti",
            Sound::Kedi => "kedi",
            Sound::System => "system",
            Sound::Bell => "bell",
            Sound::Off => "off",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScenesConfig {
    pub splash: bool,
    /// 0 turns the idle scene off.
    pub idle_minutes: u32,
    pub pool: Vec<String>,
}

impl Default for ScenesConfig {
    fn default() -> Self {
        ScenesConfig {
            splash: true,
            idle_minutes: 10,
            pool: SCENES.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotifyConfig {
    /// When an agent is done.
    pub done_sound: Sound,
    /// When an agent waits for you: a question, a permission.
    pub waiting_sound: Sound,
    pub desktop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreesConfig {
    pub location: String,
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        WorktreesConfig {
            location: "sibling".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentsConfig {
    pub default: Harness,
    pub new_worktree_by_default: bool,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            default: Harness::Claude,
            new_worktree_by_default: false,
        }
    }
}

/// Key → action overrides, as written: the TUI knows the key syntax and the actions,
/// and checks them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeysConfig {
    pub grid: BTreeMap<String, String>,
    pub focus: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub prefix: String,
    pub pane_position: PanePosition,
    pub theme: String,
    pub colors: ColorDepth,
    pub animations: bool,
    /// termist takes the mouse: the wheel scrolls a session's history and a drag
    /// copies from the pane. Off, the terminal keeps it (its own selection).
    pub mouse: bool,
    pub editor: Option<String>,
    pub scenes: ScenesConfig,
    pub notify: NotifyConfig,
    pub worktrees: WorktreesConfig,
    pub agents: AgentsConfig,
    pub keys: KeysConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: "C-a".into(),
            pane_position: PanePosition::Auto,
            theme: "uskudar".into(),
            colors: ColorDepth::Auto,
            animations: true,
            mouse: true,
            editor: None,
            scenes: ScenesConfig::default(),
            notify: NotifyConfig {
                done_sound: Sound::Off,
                waiting_sound: Sound::Off,
                desktop: true,
            },
            worktrees: WorktreesConfig::default(),
            agents: AgentsConfig::default(),
            keys: KeysConfig::default(),
        }
    }
}

/// Something in a config file that was not used as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// Where: `"scenes.idle_minutes"`, or the file for a syntax error.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl Config {
    /// Reads `main` (config.toml) with `local` (config.local.toml) over it, table by
    /// table. Never fails: whatever cannot be used is reported and left at its default.
    pub fn parse(main: &str, local: Option<&str>) -> (Config, Vec<Problem>) {
        let mut problems = Vec::new();
        let mut table = read_table("config.toml", main, &mut problems);
        if let Some(local) = local {
            merge(
                &mut table,
                read_table("config.local.toml", local, &mut problems),
            );
        }
        let config = Reader {
            problems: &mut problems,
        }
        .config(table);
        (config, problems)
    }
}

fn read_table(file: &str, text: &str, problems: &mut Vec<Problem>) -> Table {
    text.parse::<Table>().unwrap_or_else(|e| {
        problems.push(Problem {
            path: file.into(),
            message: format!("not valid TOML, ignored: {}", e.message()),
        });
        Table::new()
    })
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

struct Reader<'a> {
    problems: &'a mut Vec<Problem>,
}

impl Reader<'_> {
    fn problem(&mut self, path: &str, message: impl Into<String>) {
        self.problems.push(Problem {
            path: path.into(),
            message: message.into(),
        });
    }

    fn config(&mut self, mut t: Table) -> Config {
        let d = Config::default();
        let config = Config {
            prefix: self.string(&mut t, "", "prefix").unwrap_or(d.prefix),
            pane_position: self
                .choice(
                    &mut t,
                    "",
                    "pane_position",
                    &PanePosition::ALL,
                    PanePosition::id,
                )
                .unwrap_or(d.pane_position),
            theme: self.theme(&mut t).unwrap_or(d.theme),
            colors: self
                .choice(&mut t, "", "colors", &ColorDepth::ALL, ColorDepth::id)
                .unwrap_or(d.colors),
            animations: self.bool(&mut t, "", "animations").unwrap_or(d.animations),
            mouse: self.bool(&mut t, "", "mouse").unwrap_or(d.mouse),
            editor: self.string(&mut t, "", "editor").filter(|e| !e.is_empty()),
            scenes: self.scenes(&mut t),
            notify: self.notify(&mut t),
            worktrees: self.worktrees(&mut t),
            agents: self.agents(&mut t),
            keys: self.keys(&mut t),
        };
        self.unknown("", t);
        config
    }

    fn scenes(&mut self, t: &mut Table) -> ScenesConfig {
        let d = ScenesConfig::default();
        let Some(mut s) = self.table(t, "", "scenes") else {
            return d;
        };
        let pool = self.pool(&mut s);
        let scenes = ScenesConfig {
            splash: self.bool(&mut s, "scenes", "splash").unwrap_or(d.splash),
            idle_minutes: self
                .minutes(&mut s, "scenes", "idle_minutes")
                .unwrap_or(d.idle_minutes),
            pool: pool.unwrap_or(d.pool),
        };
        self.unknown("scenes", s);
        scenes
    }

    fn pool(&mut self, s: &mut Table) -> Option<Vec<String>> {
        let value = s.remove("pool")?;
        let Value::Array(items) = value else {
            self.problem("scenes.pool", "should be a list of scene names");
            return None;
        };
        let mut pool = Vec::new();
        for item in items {
            match item.as_str() {
                Some(name) if SCENES.contains(&name) => pool.push(name.to_string()),
                _ => self.problem(
                    "scenes.pool",
                    format!("unknown scene {item}; scenes: {}", SCENES.join(", ")),
                ),
            }
        }
        if pool.is_empty() {
            self.problem("scenes.pool", "no scene left, using all of them");
            return None;
        }
        Some(pool)
    }

    fn notify(&mut self, t: &mut Table) -> NotifyConfig {
        let d = Config::default().notify;
        let Some(mut n) = self.table(t, "", "notify") else {
            return d;
        };
        // Earlier releases had one `sounds` setting for both alerts; it still serves an
        // alert without its own.
        let both = self.sound(&mut n, "sounds");
        let notify = NotifyConfig {
            done_sound: self
                .sound(&mut n, "done_sound")
                .or(both)
                .unwrap_or(d.done_sound),
            waiting_sound: self
                .sound(&mut n, "waiting_sound")
                .or(both)
                .unwrap_or(d.waiting_sound),
            desktop: self.bool(&mut n, "notify", "desktop").unwrap_or(d.desktop),
        };
        self.unknown("notify", n);
        notify
    }

    fn worktrees(&mut self, t: &mut Table) -> WorktreesConfig {
        let d = WorktreesConfig::default();
        let Some(mut w) = self.table(t, "", "worktrees") else {
            return d;
        };
        let location = match self.string(&mut w, "worktrees", "location") {
            Some(l) if l == "sibling" => l,
            Some(other) => {
                self.problem(
                    "worktrees.location",
                    format!("unknown location \"{other}\"; locations: sibling"),
                );
                d.location
            }
            None => d.location,
        };
        self.unknown("worktrees", w);
        WorktreesConfig { location }
    }

    fn agents(&mut self, t: &mut Table) -> AgentsConfig {
        let d = AgentsConfig::default();
        let Some(mut a) = self.table(t, "", "agents") else {
            return d;
        };
        let agents = AgentsConfig {
            default: self
                .choice(&mut a, "agents", "default", &Harness::ALL, Harness::id)
                .unwrap_or(d.default),
            new_worktree_by_default: self
                .bool(&mut a, "agents", "new_worktree_by_default")
                .unwrap_or(d.new_worktree_by_default),
        };
        self.unknown("agents", a);
        agents
    }

    fn keys(&mut self, t: &mut Table) -> KeysConfig {
        let Some(mut k) = self.table(t, "", "keys") else {
            return KeysConfig::default();
        };
        let keys = KeysConfig {
            grid: self.bindings(&mut k, "grid"),
            focus: self.bindings(&mut k, "focus"),
        };
        self.unknown("keys", k);
        keys
    }

    fn bindings(&mut self, k: &mut Table, name: &str) -> BTreeMap<String, String> {
        let path = format!("keys.{name}");
        let Some(table) = self.table(k, "keys", name) else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for (key, value) in table {
            match value {
                Value::String(action) => {
                    out.insert(key, action);
                }
                other => self.problem(
                    &format!("{path}.{key}"),
                    format!("should be an action name in quotes, not {other}"),
                ),
            }
        }
        out
    }

    fn theme(&mut self, t: &mut Table) -> Option<String> {
        let theme = self.string(t, "", "theme")?;
        if THEMES.contains(&theme.as_str()) {
            return Some(theme);
        }
        let message = if LATER_THEMES.contains(&theme.as_str()) {
            format!(
                "\"{theme}\" is not in this version yet; themes: {}",
                THEMES.join(", ")
            )
        } else {
            format!("unknown theme \"{theme}\"; themes: {}", THEMES.join(", "))
        };
        self.problem("theme", message);
        None
    }

    fn table(&mut self, t: &mut Table, parent: &str, key: &str) -> Option<Table> {
        match t.remove(key)? {
            Value::Table(table) => Some(table),
            _ => {
                self.problem(&join(parent, key), "should be a table");
                None
            }
        }
    }

    fn string(&mut self, t: &mut Table, parent: &str, key: &str) -> Option<String> {
        match t.remove(key)? {
            Value::String(s) => Some(s),
            other => {
                self.problem(&join(parent, key), format!("should be text, not {other}"));
                None
            }
        }
    }

    fn bool(&mut self, t: &mut Table, parent: &str, key: &str) -> Option<bool> {
        match t.remove(key)? {
            Value::Boolean(b) => Some(b),
            other => {
                self.problem(
                    &join(parent, key),
                    format!("should be true or false, not {other}"),
                );
                None
            }
        }
    }

    fn minutes(&mut self, t: &mut Table, parent: &str, key: &str) -> Option<u32> {
        match t.remove(key)? {
            Value::Integer(n) if (0..=24 * 60).contains(&n) => Some(n as u32),
            other => {
                self.problem(
                    &join(parent, key),
                    format!("should be a number of minutes from 0 to 1440, not {other}"),
                );
                None
            }
        }
    }

    fn choice<T: Copy>(
        &mut self,
        t: &mut Table,
        parent: &str,
        key: &str,
        all: &[T],
        id: fn(T) -> &'static str,
    ) -> Option<T> {
        let path = join(parent, key);
        let text = self.string(t, parent, key)?;
        let found = all.iter().copied().find(|v| id(*v) == text);
        if found.is_none() {
            let ids: Vec<_> = all.iter().map(|v| id(*v)).collect();
            self.problem(
                &path,
                format!("unknown value \"{text}\"; one of: {}", ids.join(", ")),
            );
        }
        found
    }

    /// A sound in `[notify]`; `istanbul`, the martı's name before there was a cat, too.
    fn sound(&mut self, n: &mut Table, key: &str) -> Option<Sound> {
        if n.get(key).and_then(Value::as_str) == Some("istanbul") {
            n.insert(key.into(), Value::String(Sound::Marti.id().into()));
        }
        self.choice(n, "notify", key, &Sound::ALL, Sound::id)
    }

    /// Whatever is left in `t` was not read: report each key.
    fn unknown(&mut self, parent: &str, t: Table) {
        for key in t.keys() {
            self.problem(&join(parent, key), "unknown setting, ignored");
        }
    }
}

fn join(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(main: &str) -> (Config, Vec<String>) {
        let (config, problems) = Config::parse(main, None);
        (config, problems.iter().map(|p| p.to_string()).collect())
    }

    /// The example config of the docs, every key at its default.
    const DOCUMENTED_DEFAULTS: &str = r#"
prefix = "C-a"
pane_position = "auto"          # auto | bottom | right
theme = "uskudar"
animations = true
# editor = "code"

[scenes]
splash = true
idle_minutes = 10               # 0 = off
pool = ["galata", "kiz-kulesi", "ayasofya", "kopru", "vapur", "yerebatan"]

[notify]
done_sound = "off"              # off | marti (a seagull) | kedi (a cat) | system | bell
waiting_sound = "off"           # the same, for an agent that asks you something
desktop = true

[worktrees]
location = "sibling"

[agents]
default = "claude"
new_worktree_by_default = false

[keys.grid]
# "p" = "quick_prompt"
"#;

    #[test]
    fn an_empty_file_and_the_documented_defaults_are_the_same_config() {
        let (empty, problems) = parse("");
        assert!(problems.is_empty());
        assert_eq!(empty, Config::default());
        let (documented, problems) = parse(DOCUMENTED_DEFAULTS);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(documented, Config::default());
    }

    #[test]
    fn values_are_read() {
        let (c, problems) = parse(
            r#"
prefix = "C-Space"
pane_position = "right"
theme = "moda"
colors = "256"
animations = false
editor = "zed"
[scenes]
idle_minutes = 0
pool = ["galata", "vapur"]
[notify]
done_sound = "kedi"
waiting_sound = "bell"
[agents]
default = "codex"
[keys.grid]
"g" = "quick_prompt"
"p" = "none"
[keys.focus]
"z" = "help"
"#,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(c.prefix, "C-Space");
        assert_eq!(c.pane_position, PanePosition::Right);
        assert_eq!(c.theme, "moda");
        assert_eq!(c.colors, ColorDepth::Ansi256);
        assert!(!c.animations);
        assert_eq!(c.editor.as_deref(), Some("zed"));
        assert_eq!(c.scenes.idle_minutes, 0);
        assert_eq!(c.scenes.pool, ["galata", "vapur"]);
        assert!(c.scenes.splash, "untouched keys keep their default");
        assert_eq!(c.notify.done_sound, Sound::Kedi);
        assert_eq!(c.notify.waiting_sound, Sound::Bell);
        assert_eq!(c.agents.default, Harness::Codex);
        assert_eq!(c.keys.grid["g"], "quick_prompt");
        assert_eq!(c.keys.grid["p"], "none");
        assert_eq!(c.keys.focus["z"], "help");
    }

    #[test]
    fn the_local_file_wins_key_by_key() {
        let (c, problems) = Config::parse(
            "theme = \"moda\"\n[scenes]\nsplash = false\nidle_minutes = 3\n",
            Some("theme = \"terminal\"\n[scenes]\nidle_minutes = 20\n"),
        );
        assert!(problems.is_empty());
        assert_eq!(c.theme, "terminal");
        assert_eq!(c.scenes.idle_minutes, 20);
        assert!(!c.scenes.splash, "a table merges, it is not replaced");
    }

    #[test]
    fn a_bad_value_is_reported_and_only_that_setting_falls_back() {
        let (c, problems) = parse(
            r#"
theme = "kadikoy"
animations = "yes"
pane_position = "left"
colors = 16
[scenes]
idle_minutes = -1
pool = ["galata", "eminonu"]
[agents]
default = "aider"
"#,
        );
        assert_eq!(c.theme, "uskudar");
        assert!(c.animations);
        assert_eq!(c.pane_position, PanePosition::Auto);
        assert_eq!(c.colors, ColorDepth::Auto);
        assert_eq!(c.scenes.idle_minutes, 10);
        assert_eq!(c.scenes.pool, ["galata"], "the known scene stays");
        assert_eq!(c.agents.default, Harness::Claude);
        assert_eq!(problems.len(), 7, "{problems:#?}");
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("theme: \"kadikoy\" is not in this version yet"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("animations: should be true or false"))
        );
        assert!(problems.iter().any(|p| p.contains("eminonu")));
    }

    #[test]
    fn the_old_sounds_setting_serves_both_alerts_unless_one_has_its_own() {
        let (c, problems) = parse("[notify]\nsounds = \"istanbul\"\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(c.notify.done_sound, Sound::Marti);
        assert_eq!(c.notify.waiting_sound, Sound::Marti);
        let (c, problems) = Config::parse(
            "[notify]\nsounds = \"system\"\n",
            Some("[notify]\nwaiting_sound = \"kedi\"\n"),
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(c.notify.done_sound, Sound::System);
        assert_eq!(c.notify.waiting_sound, Sound::Kedi);
    }

    #[test]
    fn an_unknown_sound_names_the_sounds() {
        let (c, problems) = parse("[notify]\ndone_sound = \"vapur\"\n");
        assert_eq!(c.notify.done_sound, Sound::Off);
        assert_eq!(
            problems,
            ["notify.done_sound: unknown value \"vapur\"; one of: marti, kedi, system, bell, off"]
        );
    }

    #[test]
    fn unknown_keys_are_named_with_their_table() {
        let (c, problems) = parse("them = \"moda\"\n[notify]\nsound = \"off\"\n[mystery]\nx = 1\n");
        assert_eq!(c, Config::default());
        assert_eq!(
            problems,
            [
                "notify.sound: unknown setting, ignored",
                "mystery: unknown setting, ignored",
                "them: unknown setting, ignored",
            ]
        );
    }

    #[test]
    fn broken_toml_is_one_problem_and_the_defaults() {
        let (c, problems) = parse("theme = \"moda\n[scenes");
        assert_eq!(c, Config::default());
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with("config.toml: not valid TOML"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_broken_local_file_leaves_the_main_one_in_force() {
        let (c, problems) = Config::parse("theme = \"moda\"", Some("theme = "));
        assert_eq!(c.theme, "moda");
        assert!(problems[0].path == "config.local.toml");
    }

    #[test]
    fn a_key_binding_that_is_not_text_is_reported() {
        let (c, problems) = parse("[keys.grid]\n\"g\" = 3\n\"h\" = \"left\"\n");
        assert_eq!(c.keys.grid.len(), 1);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("keys.grid.g: should be an action name"));
    }
}
