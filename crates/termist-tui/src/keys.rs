//! What each key does: the actions of the grid and of focus mode (after the prefix),
//! their default keys, and the user's own from `[keys.grid]` and `[keys.focus]`.
//!
//! Two keys are never rebound: Ctrl+Q (out of anything, from anywhere) and Ctrl+C (quit).
//! Text boxes and lists keep their keys.
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt;
use termist_core::config::{KeysConfig, Problem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
    Grid,
    /// Focus mode, after the prefix.
    Focus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Focus,
    /// Back to the grid from focus mode.
    Grid,
    NewSession,
    QuickPrompt,
    /// A new task like the selected card's: its CLI, model and effort, in its worktree.
    SameTask,
    /// Removes the selection's worktree (its branch stays), after asking.
    RemoveWorktree,
    /// The project's worktrees: show, hide, remove.
    Worktrees,
    /// The selected card's folder's diff (Ayna).
    LocalDiff,
    /// Agent setups kept for again: one opens the new-task prompt ready.
    Presets,
    /// lazygit in the selected card's folder, as a card for a while.
    Lazygit,
    /// The selected card's folder in the user's editor.
    Editor,
    /// A file of the repo, opened in the editor.
    FindFile,
    /// The lines with some text (`git grep`), opened at the line.
    Grep,
    NewShell,
    FollowUp,
    Rename,
    Archive,
    ArchiveView,
    /// The pull requests of the project (the view, or back to the grid).
    PullRequests,
    /// The selected card's pull request, in the browser.
    PullRequestInBrowser,
    /// The project's open issues (the pull request view's Issues tab).
    Issues,
    /// Read GitHub again now.
    RefreshGitHub,
    Palette,
    HalfPageDown,
    HalfPageUp,
    Kill,
    NextTab,
    PrevTab,
    /// The nth open project tab, 1 to 9.
    Tab(u8),
    OpenProject,
    CloseTab,
    NextAttention,
    PrevAttention,
    Left,
    Down,
    Up,
    Right,
    Help,
    Settings,
    /// Focus mode: the pane under the cards or right of them, until termist quits.
    TogglePane,
    /// Back through the pane's history, a page at a time; the scroll keys take over.
    ScrollBack,
}

use Action::*;

/// Every grid action, in the order help and settings list them.
pub const GRID_ACTIONS: &[Action] = &[
    QuickPrompt,
    FollowUp,
    Focus,
    Palette,
    NextAttention,
    PrevAttention,
    NewSession,
    NewShell,
    SameTask,
    Worktrees,
    RemoveWorktree,
    LocalDiff,
    Presets,
    Lazygit,
    Editor,
    FindFile,
    Grep,
    Left,
    Down,
    Up,
    Right,
    HalfPageDown,
    HalfPageUp,
    ScrollBack,
    NextTab,
    PrevTab,
    Tab(1),
    Tab(2),
    Tab(3),
    Tab(4),
    Tab(5),
    Tab(6),
    Tab(7),
    Tab(8),
    Tab(9),
    OpenProject,
    CloseTab,
    Rename,
    Archive,
    ArchiveView,
    PullRequests,
    PullRequestInBrowser,
    Issues,
    RefreshGitHub,
    Kill,
    Settings,
    Help,
    Quit,
];

/// Every focus-mode action (after the prefix), in the same order.
pub const FOCUS_ACTIONS: &[Action] = &[
    Grid,
    QuickPrompt,
    Palette,
    PullRequests,
    Issues,
    LocalDiff,
    Presets,
    Lazygit,
    Editor,
    FindFile,
    Grep,
    NextAttention,
    PrevAttention,
    NewSession,
    NewShell,
    Left,
    Down,
    Up,
    Right,
    TogglePane,
    ScrollBack,
    Help,
];

impl Action {
    /// The name config.toml uses; it never changes.
    pub fn id(self) -> &'static str {
        match self {
            Quit => "quit",
            Focus => "focus",
            Grid => "grid",
            NewSession => "new_session",
            QuickPrompt => "quick_prompt",
            SameTask => "same_task",
            Worktrees => "worktrees",
            RemoveWorktree => "remove_worktree",
            LocalDiff => "local_diff",
            Presets => "presets",
            Lazygit => "lazygit",
            Editor => "editor",
            FindFile => "find_file",
            Grep => "grep",
            NewShell => "new_shell",
            FollowUp => "follow_up",
            Rename => "rename",
            Archive => "archive",
            ArchiveView => "archive_view",
            PullRequests => "pull_requests",
            PullRequestInBrowser => "pull_request_in_browser",
            Issues => "issues",
            RefreshGitHub => "refresh_github",
            Palette => "palette",
            HalfPageDown => "half_page_down",
            HalfPageUp => "half_page_up",
            Kill => "kill",
            NextTab => "next_tab",
            PrevTab => "prev_tab",
            Tab(n) => TAB_IDS[(n.clamp(1, 9) - 1) as usize],
            OpenProject => "open_project",
            CloseTab => "close_tab",
            NextAttention => "next_attention",
            PrevAttention => "prev_attention",
            Left => "left",
            Down => "down",
            Up => "up",
            Right => "right",
            Help => "help",
            Settings => "settings",
            TogglePane => "toggle_pane",
            ScrollBack => "scroll_back",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        GRID_ACTIONS
            .iter()
            .chain(FOCUS_ACTIONS)
            .copied()
            .find(|a| a.id() == id)
    }

    /// A few words for the footer.
    pub fn hint(self) -> &'static str {
        match self {
            Quit => "quit",
            Focus => "focus",
            Grid => "grid",
            NewSession => "agent",
            QuickPrompt => "new task",
            SameTask => "same task",
            Worktrees => "worktrees",
            RemoveWorktree => "remove worktree",
            LocalDiff => "local diff",
            Presets => "presets",
            Lazygit => "lazygit",
            Editor => "editor",
            FindFile => "find a file",
            Grep => "find text",
            NewShell => "shell",
            FollowUp => "follow-up",
            Rename => "rename",
            Archive => "archive",
            ArchiveView => "archived",
            PullRequests => "pull requests",
            PullRequestInBrowser => "PR in browser",
            Issues => "issues",
            RefreshGitHub => "refresh",
            Palette => "sessions",
            HalfPageDown => "half page down",
            HalfPageUp => "half page up",
            Kill => "kill",
            NextTab => "next tab",
            PrevTab => "previous tab",
            Tab(_) => "tab",
            OpenProject => "open",
            CloseTab => "close tab",
            NextAttention => "next●",
            PrevAttention => "previous●",
            Left | Down | Up | Right => "move",
            Help => "help",
            Settings => "settings",
            TogglePane => "pane",
            ScrollBack => "scroll back",
        }
    }

    /// What it does, for help and settings.
    pub fn label(self) -> &'static str {
        match self {
            Quit => "quit termist (sessions keep running)",
            Focus => "type into the selected session",
            Grid => "back to the grid",
            NewSession => "new agent session",
            QuickPrompt => "new task: prompt, CLI, model",
            SameTask => "new task like the card, in its worktree",
            Worktrees => "the worktrees: show, hide, remove",
            RemoveWorktree => "remove the worktree; the branch stays",
            LocalDiff => "what the card's branch changed, file by file",
            Presets => "your agent setups: CLI, model, words (^S saves one)",
            Lazygit => "lazygit in the card's folder, as a card",
            Editor => "the card's folder in your editor",
            FindFile => "a file of the repo, opened in your editor",
            Grep => "lines with some text (git grep), opened at the line",
            NewShell => "new shell",
            FollowUp => "send the next instruction without entering",
            Rename => "rename the card",
            Archive => "archive the card",
            ArchiveView => "show archived cards",
            PullRequests => "the project's pull requests",
            PullRequestInBrowser => "the card's pull request in the browser",
            Issues => "the project's open issues",
            RefreshGitHub => "read GitHub again now",
            Palette => "find a session",
            HalfPageDown => "half a page down",
            HalfPageUp => "half a page up",
            Kill => "kill the session",
            NextTab => "next project tab",
            PrevTab => "previous project tab",
            Tab(n) => TAB_LABELS[(n.clamp(1, 9) - 1) as usize],
            OpenProject => "open a project",
            CloseTab => "close the project tab",
            NextAttention => "next session that needs you",
            PrevAttention => "previous session that needs you",
            Left => "card to the left",
            Down => "card below",
            Up => "card above",
            Right => "card to the right",
            Help => "this help",
            Settings => "settings: theme, colours, prefix, pane, keys",
            TogglePane => "pane under or right of the cards, until you quit",
            ScrollBack => "scroll back through the session's output",
        }
    }
}

const TAB_IDS: [&str; 9] = [
    "tab_1", "tab_2", "tab_3", "tab_4", "tab_5", "tab_6", "tab_7", "tab_8", "tab_9",
];
const TAB_LABELS: [&str; 9] = [
    "project tab 1",
    "project tab 2",
    "project tab 3",
    "project tab 4",
    "project tab 5",
    "project tab 6",
    "project tab 7",
    "project tab 8",
    "project tab 9",
];

/// A key as written in config.toml: `p`, `P`, `C-d`, `M-x`, `C-Space`, `Enter`, `F5`,
/// `?`. A character key ignores Shift (`P` is already the shifted `p`); Ctrl and Alt
/// must match exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeySpec {
    code: KeyCode,
    mods: KeyModifiers,
}

const NAMED: [(&str, KeyCode); 16] = [
    ("Enter", KeyCode::Enter),
    ("Esc", KeyCode::Esc),
    ("Tab", KeyCode::Tab),
    ("BackTab", KeyCode::BackTab),
    ("Space", KeyCode::Char(' ')),
    ("Backspace", KeyCode::Backspace),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("PageUp", KeyCode::PageUp),
    ("PageDown", KeyCode::PageDown),
    ("Delete", KeyCode::Delete),
    ("Insert", KeyCode::Insert),
];

impl KeySpec {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> KeySpec {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            KeyCode::Char(c) => {
                mods.remove(KeyModifiers::SHIFT);
                // AltGr comes as Ctrl+Alt (crossterm on Windows): `[` is AltGr+8 on many
                // layouts, and is still `[`. Letters keep Ctrl+Alt, so C-M-k stays itself.
                let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
                if mods.contains(both) && !c.is_ascii_alphabetic() {
                    mods.remove(both);
                }
                // Ctrl+letter comes as the lower-case letter from most terminals.
                if mods.contains(KeyModifiers::CONTROL) {
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            other => other,
        };
        KeySpec { code, mods }
    }

    pub fn of(key: &KeyEvent) -> KeySpec {
        KeySpec::new(key.code, key.modifiers)
    }

    pub fn parse(text: &str) -> Result<KeySpec, String> {
        let mut rest = text;
        let mut mods = KeyModifiers::NONE;
        loop {
            let m = match rest.get(..2) {
                Some("C-") => KeyModifiers::CONTROL,
                Some("M-") => KeyModifiers::ALT,
                Some("S-") => KeyModifiers::SHIFT,
                _ => break,
            };
            if rest.len() == 2 {
                break; // "C-" alone is not a key
            }
            mods |= m;
            rest = &rest[2..];
        }
        let code = if let Some((_, code)) = NAMED.iter().find(|(n, _)| *n == rest) {
            *code
        } else if let Some(n) = rest
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=24).contains(n))
        {
            KeyCode::F(n)
        } else {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() && c != ' ' => KeyCode::Char(c),
                _ => {
                    return Err(format!(
                        "\"{text}\" is not a key (e.g. p, P, C-d, M-x, Enter, F5)"
                    ));
                }
            }
        };
        Ok(KeySpec::new(code, mods))
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        KeySpec::of(key) == *self
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (m, p) in [
            (KeyModifiers::CONTROL, "C-"),
            (KeyModifiers::ALT, "M-"),
            (KeyModifiers::SHIFT, "S-"),
        ] {
            if self.mods.contains(m) {
                f.write_str(p)?;
            }
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            code => match NAMED.iter().find(|(_, c)| *c == code) {
                Some((name, _)) => f.write_str(name),
                None => write!(f, "{code:?}"),
            },
        }
    }
}

/// Never rebound: Ctrl+Q gets you out of anything, Ctrl+C quits.
fn reserved(key: &KeySpec) -> Option<&'static str> {
    let ctrl = |c| KeySpec::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    if *key == ctrl('q') {
        Some("C-q always gets you out and cannot be rebound")
    } else if *key == ctrl('c') {
        Some("C-c always quits and cannot be rebound")
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub prefix: KeySpec,
    grid: Vec<(KeySpec, Action)>,
    focus: Vec<(KeySpec, Action)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::defaults()
    }
}

impl Keymap {
    pub fn defaults() -> Keymap {
        let k = |s: &str| KeySpec::parse(s).expect("a default key");
        let mut grid: Vec<(KeySpec, Action)> = [
            ("p", QuickPrompt),
            ("P", SameTask),
            ("W", Worktrees),
            ("X", RemoveWorktree),
            ("g", LocalDiff),
            ("e", Presets),
            ("L", Lazygit),
            ("O", Editor),
            ("f", FindFile),
            ("F", Grep),
            ("Space", FollowUp),
            ("Enter", Focus),
            ("/", Palette),
            (".", NextAttention),
            (",", PrevAttention),
            ("n", NewSession),
            ("t", NewShell),
            ("h", Left),
            ("j", Down),
            ("k", Up),
            ("l", Right),
            ("C-d", HalfPageDown),
            ("C-u", HalfPageUp),
            ("PageUp", ScrollBack),
            ("]", NextTab),
            ("[", PrevTab),
            ("o", OpenProject),
            ("x", CloseTab),
            ("r", Rename),
            ("a", Archive),
            ("A", ArchiveView),
            ("v", PullRequests),
            ("V", PullRequestInBrowser),
            ("i", Issues),
            ("R", RefreshGitHub),
            ("d", Kill),
            ("s", Settings),
            ("?", Help),
            ("q", Quit),
        ]
        .into_iter()
        .map(|(key, action)| (k(key), action))
        .collect();
        for n in 1..=9u8 {
            grid.push((k(&n.to_string()), Tab(n)));
        }
        let focus = [
            ("Esc", Grid),
            ("q", Grid),
            ("p", QuickPrompt),
            ("/", Palette),
            ("v", PullRequests),
            ("i", Issues),
            ("g", LocalDiff),
            ("e", Presets),
            ("L", Lazygit),
            ("O", Editor),
            ("f", FindFile),
            ("F", Grep),
            (".", NextAttention),
            (",", PrevAttention),
            ("n", NewSession),
            ("t", NewShell),
            ("h", Left),
            ("j", Down),
            ("k", Up),
            ("l", Right),
            ("z", TogglePane),
            ("[", ScrollBack),
            ("?", Help),
        ]
        .into_iter()
        .map(|(key, action)| (k(key), action))
        .collect();
        Keymap {
            prefix: k("C-a"),
            grid,
            focus,
        }
    }

    /// The defaults with `prefix` and `keys` over them. What cannot be used is reported
    /// and left at its default.
    pub fn from_config(keys: &KeysConfig, prefix: &str) -> (Keymap, Vec<Problem>) {
        let mut map = Keymap::defaults();
        let mut problems = Vec::new();
        let mut problem = |path: String, message: String| problems.push(Problem { path, message });
        match KeySpec::parse(prefix) {
            Ok(p) => match Keymap::refuses_prefix(&p) {
                Some(why) => problem("prefix".into(), why),
                None => map.prefix = p,
            },
            Err(e) => problem("prefix".into(), e),
        }
        for (context, name, bindings) in [
            (Context::Grid, "grid", &keys.grid),
            (Context::Focus, "focus", &keys.focus),
        ] {
            for (key_text, action_text) in bindings {
                let path = format!("keys.{name}.{key_text}");
                let key = match KeySpec::parse(key_text) {
                    Ok(key) => key,
                    Err(e) => {
                        problem(path, e);
                        continue;
                    }
                };
                if let Some(why) = reserved(&key) {
                    problem(path, why.into());
                    continue;
                }
                if context == Context::Focus && key == map.prefix {
                    problem(
                        path,
                        "the prefix itself: pressed twice it goes to the session".into(),
                    );
                    continue;
                }
                if action_text == "none" {
                    map.bind(context, key, None);
                    continue;
                }
                match Action::from_id(action_text).filter(|a| actions(context).contains(a)) {
                    Some(action) => map.bind(context, key, Some(action)),
                    None => problem(
                        path,
                        format!(
                            "\"{action_text}\" is not a {name} action (termist's help lists them)"
                        ),
                    ),
                }
            }
        }
        (map, problems)
    }

    fn table(&self, context: Context) -> &Vec<(KeySpec, Action)> {
        match context {
            Context::Grid => &self.grid,
            Context::Focus => &self.focus,
        }
    }

    /// Binds `key` to `action` in `context`, or unbinds it with `None`.
    pub fn bind(&mut self, context: Context, key: KeySpec, action: Option<Action>) {
        let table = match context {
            Context::Grid => &mut self.grid,
            Context::Focus => &mut self.focus,
        };
        table.retain(|(k, _)| *k != key);
        if let Some(action) = action {
            table.push((key, action));
        }
    }

    /// Makes `keys` the only keys of `action` in `context`, taking them from whatever
    /// they were bound to.
    pub fn set_keys(&mut self, context: Context, action: Action, keys: &[KeySpec]) {
        let table = match context {
            Context::Grid => &mut self.grid,
            Context::Focus => &mut self.focus,
        };
        table.retain(|(k, a)| *a != action && !keys.contains(k));
        table.extend(keys.iter().map(|k| (*k, action)));
    }

    /// The bindings of `context` that differ from the defaults, as config.toml writes
    /// them: `(key, action id)`, `"none"` for a default key left unbound.
    pub fn overrides(&self, context: Context) -> Vec<(String, String)> {
        let defaults = Keymap::defaults();
        let (default, current) = (defaults.table(context), self.table(context));
        let mut out: Vec<(String, String)> = current
            .iter()
            .filter(|binding| !default.contains(binding))
            .map(|(k, a)| (k.to_string(), a.id().to_string()))
            .collect();
        out.extend(
            default
                .iter()
                .filter(|(k, _)| !current.iter().any(|(c, _)| c == k))
                .map(|(k, _)| (k.to_string(), "none".to_string())),
        );
        out.sort();
        out
    }

    /// Why `key` cannot be bound in `context`, if it cannot.
    pub fn refuses(&self, context: Context, key: &KeySpec) -> Option<String> {
        if let Some(why) = reserved(key) {
            return Some(why.into());
        }
        (context == Context::Focus && *key == self.prefix)
            .then(|| format!("{key} is the prefix: pressed twice it goes to the session"))
    }

    /// Why `key` cannot be the prefix, if it cannot.
    pub fn refuses_prefix(key: &KeySpec) -> Option<String> {
        if let Some(why) = reserved(key) {
            return Some(why.into());
        }
        // Enter, Tab, arrows and the rest are what agents need unchanged.
        (!key
            .mods
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT))
        .then(|| format!("{key} would no longer reach the session; use a key with C- or M-"))
    }

    /// Whether `key` reads back the same from config.toml (some keys have no name).
    pub fn writable(key: &KeySpec) -> bool {
        KeySpec::parse(&key.to_string()) == Ok(*key)
    }

    pub fn action(&self, context: Context, key: &KeyEvent) -> Option<Action> {
        let spec = KeySpec::of(key);
        self.table(context)
            .iter()
            .find(|(k, _)| *k == spec)
            .map(|(_, a)| *a)
    }

    /// The keys bound to `action`, in the order they were bound.
    pub fn keys(&self, context: Context, action: Action) -> Vec<KeySpec> {
        self.table(context)
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(k, _)| *k)
            .collect()
    }

    /// The first key of `action`, written as in config.toml.
    pub fn key(&self, context: Context, action: Action) -> Option<String> {
        self.keys(context, action).first().map(|k| k.to_string())
    }
}

/// What in `config` the keymap cannot use: the prefix and `[keys.*]`.
pub fn problems(config: &termist_core::config::Config) -> Vec<Problem> {
    Keymap::from_config(&config.keys, &config.prefix).1
}

pub fn actions(context: Context) -> &'static [Action] {
    match context {
        Context::Grid => GRID_ACTIONS,
        Context::Focus => FOCUS_ACTIONS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn keys_are_written_back_as_they_are_read() {
        for text in [
            "p", "P", "?", "/", ".", "C-d", "M-x", "C-Space", "Space", "Enter", "Esc", "F5",
            "PageDown", "C-M-k", "S-Up", "Delete", "Insert", "F24",
        ] {
            assert_eq!(KeySpec::parse(text).unwrap().to_string(), text);
        }
        for bad in ["", "pp", "F25", "Ctrl+d", "C-"] {
            assert!(KeySpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_character_ignores_shift_and_ctrl_and_alt_must_match() {
        let big_a = KeySpec::parse("A").unwrap();
        assert!(big_a.matches(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)));
        assert!(big_a.matches(&ev(KeyCode::Char('A'), KeyModifiers::NONE)));
        assert!(!big_a.matches(&ev(
            KeyCode::Char('A'),
            KeyModifiers::SHIFT | KeyModifiers::CONTROL
        )));
        let h = KeySpec::parse("h").unwrap();
        assert!(!h.matches(&ev(KeyCode::Char('h'), KeyModifiers::CONTROL)));
        let question = KeySpec::parse("?").unwrap();
        assert!(question.matches(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        let ctrl_space = KeySpec::parse("C-Space").unwrap();
        assert!(ctrl_space.matches(&ev(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    }

    #[test]
    fn the_defaults_are_todays_keys() {
        let m = Keymap::defaults();
        let grid = |c| m.action(Context::Grid, &ev(KeyCode::Char(c), KeyModifiers::NONE));
        assert_eq!(grid('p'), Some(QuickPrompt));
        assert_eq!(grid(' '), Some(FollowUp));
        assert_eq!(grid('3'), Some(Tab(3)));
        assert_eq!(grid('A'), Some(ArchiveView));
        assert_eq!(grid('v'), Some(PullRequests));
        assert_eq!(grid('i'), Some(Issues));
        assert_eq!(grid('R'), Some(RefreshGitHub));
        assert_eq!(
            m.action(Context::Focus, &ev(KeyCode::Char('v'), KeyModifiers::NONE)),
            Some(PullRequests)
        );
        assert_eq!(
            m.action(
                Context::Grid,
                &ev(KeyCode::Char('d'), KeyModifiers::CONTROL)
            ),
            Some(HalfPageDown)
        );
        assert_eq!(
            m.action(Context::Focus, &ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Grid)
        );
        assert_eq!(m.prefix.to_string(), "C-a");
        for a in GRID_ACTIONS {
            assert!(!m.keys(Context::Grid, *a).is_empty(), "{a:?} has a key");
            assert_eq!(Action::from_id(a.id()), Some(*a));
        }
    }

    fn config(grid: &[(&str, &str)], focus: &[(&str, &str)]) -> KeysConfig {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        KeysConfig {
            grid: map(grid),
            focus: map(focus),
        }
    }

    #[test]
    fn the_config_rebinds_and_unbinds() {
        let (m, problems) = Keymap::from_config(
            &config(&[("g", "quick_prompt"), ("p", "none")], &[("z", "palette")]),
            "C-Space",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let none = KeyModifiers::NONE;
        assert_eq!(
            m.action(Context::Grid, &ev(KeyCode::Char('g'), none)),
            Some(QuickPrompt)
        );
        assert_eq!(m.action(Context::Grid, &ev(KeyCode::Char('p'), none)), None);
        assert_eq!(
            m.action(Context::Focus, &ev(KeyCode::Char('z'), none)),
            Some(Palette)
        );
        assert_eq!(m.key(Context::Grid, QuickPrompt).as_deref(), Some("g"));
        assert_eq!(m.prefix.to_string(), "C-Space");
    }

    #[test]
    fn what_cannot_be_bound_is_reported() {
        let (m, problems) = Keymap::from_config(
            &config(
                &[
                    ("C-q", "palette"),
                    ("C-c", "palette"),
                    ("g", "fly"),
                    ("pp", "palette"),
                    ("y", "grid"),
                ],
                &[("C-a", "palette")],
            ),
            "x",
        );
        let text: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        assert_eq!(text.len(), 7, "{text:#?}");
        assert!(
            text[0].starts_with("prefix: x would no longer reach the session"),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|t| t.starts_with("keys.grid.C-q: C-q always gets you out"))
        );
        assert!(
            text.iter()
                .any(|t| t.starts_with("keys.grid.g: \"fly\" is not a grid action"))
        );
        assert!(
            text.iter()
                .any(|t| t.starts_with("keys.grid.y: \"grid\" is not a grid action"))
        );
        assert!(
            text.iter()
                .any(|t| t.starts_with("keys.focus.C-a: the prefix itself"))
        );
        assert_eq!(m, Keymap::defaults(), "nothing of it was used");
    }

    #[test]
    fn overrides_are_what_differs_from_the_defaults() {
        let mut m = Keymap::defaults();
        assert!(m.overrides(Context::Grid).is_empty());
        let b = KeySpec::parse("b").unwrap();
        m.set_keys(Context::Grid, QuickPrompt, &[b]);
        assert_eq!(
            m.overrides(Context::Grid),
            [
                ("b".to_string(), "quick_prompt".to_string()),
                ("p".into(), "none".into())
            ]
        );
        let (read_back, problems) = Keymap::from_config(
            &KeysConfig {
                grid: m.overrides(Context::Grid).into_iter().collect(),
                ..Default::default()
            },
            "C-a",
        );
        assert!(problems.is_empty());
        assert_eq!(read_back.keys(Context::Grid, QuickPrompt), [b]);
        m.set_keys(Context::Grid, QuickPrompt, &[KeySpec::parse("p").unwrap()]);
        assert!(m.overrides(Context::Grid).is_empty(), "back to the default");
    }

    #[test]
    fn a_key_taken_for_another_action_leaves_the_old_one() {
        let mut m = Keymap::defaults();
        let k = KeySpec::parse("k").unwrap();
        m.set_keys(Context::Grid, Palette, &[k]);
        assert_eq!(m.keys(Context::Grid, Up), []);
        assert_eq!(m.keys(Context::Grid, Palette), [k]);
    }

    #[test]
    fn altgr_characters_are_the_characters() {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let m = Keymap::defaults();
        assert_eq!(
            m.action(Context::Grid, &ev(KeyCode::Char(']'), altgr)),
            Some(NextTab)
        );
        assert_eq!(
            m.action(Context::Grid, &ev(KeyCode::Char('['), altgr)),
            Some(PrevTab)
        );
        assert_eq!(
            KeySpec::of(&ev(KeyCode::Char('k'), altgr)).to_string(),
            "C-M-k",
            "a letter keeps both"
        );
    }

    #[test]
    fn the_prefix_needs_ctrl_or_alt() {
        for bad in ["Enter", "Tab", "Backspace", "Up", "F5", "x"] {
            let key = KeySpec::parse(bad).unwrap();
            assert!(Keymap::refuses_prefix(&key).is_some(), "{bad}");
        }
        for good in ["C-a", "C-Space", "M-a", "C-F5"] {
            assert_eq!(
                Keymap::refuses_prefix(&KeySpec::parse(good).unwrap()),
                None,
                "{good}"
            );
        }
        assert!(Keymap::writable(&KeySpec::parse("Delete").unwrap()));
        assert!(!Keymap::writable(&KeySpec::new(
            KeyCode::CapsLock,
            KeyModifiers::NONE
        )));
    }
}
