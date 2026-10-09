use crate::ids::{ProjectId, SessionId};
use crate::status::AgentStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Harness {
    Claude,
    Codex,
    OpenCode,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::OpenCode];

    pub fn id(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        }
    }

    /// The executable name looked up on PATH.
    pub fn program(self) -> &'static str {
        self.id()
    }

    pub fn from_id(s: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.id() == s)
    }

    /// The reasoning-effort levels the CLI takes as a flag; empty when it has none.
    pub fn efforts(self) -> &'static [&'static str] {
        match self {
            Harness::Claude => &["low", "medium", "high", "xhigh", "max"],
            Harness::Codex => &["low", "medium", "high"],
            Harness::OpenCode => &[],
        }
    }
}

/// Effort levels any agent CLI is known to take, from least to most.
pub const EFFORT_LEVELS: [&str; 7] = ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

/// What a new agent session starts with. `None` leaves the choice to the CLI: no
/// flag is passed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOptions {
    pub harness: Harness,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// A model an agent CLI offers, for the quick prompt's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// What goes to the CLI's model flag.
    pub id: String,
    /// What the list shows.
    pub label: String,
    /// The effort levels this model takes; empty: the harness's own list.
    pub efforts: Vec<String>,
}

/// The colours an agent is told about when it asks (OSC 10, 11 and 4): its default
/// foreground and background and, when a theme sets them, the 16 ANSI colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermColors {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub ansi: Option<[(u8, u8, u8); 16]>,
}

impl Default for TermColors {
    /// A dark terminal, for when nothing better is known.
    fn default() -> Self {
        TermColors {
            fg: (0xd8, 0xd8, 0xd8),
            bg: (0x1e, 0x1e, 0x1e),
            ansi: None,
        }
    }
}

/// Whether the daemon found a harness's CLI when it started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub harness: Harness,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Agent {
        harness: Harness,
    },
    Shell,
    /// A program for a while (lazygit, a terminal editor): gone when it ends, never
    /// kept.
    Tool {
        program: String,
        args: Vec<String>,
    },
}

impl SessionKind {
    /// What a tool opened, for its card: a file relative to `cwd`, at its line
    /// (`src/a.rs:42`); `None` for anything else.
    pub fn opens(&self, cwd: &std::path::Path) -> Option<String> {
        let SessionKind::Tool { args, .. } = self else {
            return None;
        };
        let line = args.iter().find_map(|a| a.strip_prefix('+'));
        let file = args
            .iter()
            .rev()
            .find(|a| !a.starts_with('+') && !a.starts_with('-') && *a != ".")?;
        let shown = std::path::Path::new(file)
            .strip_prefix(cwd)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file.clone());
        Some(match line {
            Some(n) => format!("{shown}:{n}"),
            None => shown,
        })
    }

    /// Short label shown on cards: the harness id, "shell", or the tool's name.
    pub fn label(&self) -> &str {
        match self {
            SessionKind::Agent { harness } => harness.id(),
            SessionKind::Shell => "shell",
            SessionKind::Tool { program, .. } => {
                program.rsplit(['/', '\\']).next().unwrap_or(program)
            }
        }
    }
}

/// Editors with windows of their own; any other runs in a terminal (a card).
pub const GUI_EDITORS: &[&str] = &[
    "code",
    "code-insiders",
    "codium",
    "cursor",
    "windsurf",
    "zed",
    "subl",
    "idea",
    "goland",
    "webstorm",
    "pycharm",
    "rustrover",
    "fleet",
];

/// Whether the editor `choice` names (its program, then arguments) has a window of
/// its own; with no choice the daemon looks for GUI ones only.
pub fn gui_editor(choice: Option<&str>) -> bool {
    let Some(first) = choice.and_then(|c| c.split_whitespace().next()) else {
        return true;
    };
    let name = std::path::Path::new(first)
        .file_stem()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    GUI_EDITORS.contains(&name.as_str())
}

/// A line `git grep` found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepMatch {
    /// Relative to the repo's root.
    pub path: String,
    pub line: u32,
    /// The line, cut to a length a list can show.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
    /// Shown as a tab. Closing a project hides it; its sessions keep running.
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub project: ProjectId,
    pub kind: SessionKind,
    pub name: String,
    pub status: AgentStatus,
    /// The agent CLI's own session id (for resume), when known.
    pub agent_session_id: Option<String>,
    /// Terminal title set by the child (OSC 0/2), used as auto-title later.
    pub title: Option<String>,
    pub last_activity_ms: u64,
    /// The model and effort it was started with; `None` is the CLI's default.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Renamed by the user: the name wins over the terminal title from then on.
    pub user_named: bool,
    /// Hidden from the grid, the palette and the attention order; the record stays.
    pub archived: bool,
    /// The folder it runs in: where it started, or where the agent says it is now.
    pub cwd: PathBuf,
    /// What the daemon read about `cwd`; `None` until read.
    pub place: Option<Box<Place>>,
}

/// A worktree of one of a project's repos, as the daemon keeps and reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub repo: Option<crate::github::RepoId>,
    /// `None` when detached.
    pub branch: Option<String>,
    /// The branch it was made from, when termist made it.
    pub base: Option<String>,
    pub made_by_termist: bool,
    /// Drawn as a band even with no cards in it.
    pub shown: bool,
    /// What the branch changed since it left its base, the uncommitted too; `None` until
    /// read or when it cannot be (no merge base).
    pub stat: Option<Stat>,
    /// The last pull request seen on its branch, once it is no longer open.
    pub pr_end: Option<(u32, PrEnd)>,
    /// Files of `stat` marked reviewed that have not changed since (`g`, `Ctrl+r`).
    pub reviewed: u32,
}

/// What `g` shows: the branch since it left its base, or only what is not committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiffMode {
    #[default]
    Branch,
    Uncommitted,
}

/// Where reading something local stands; `Failed` says why in git's words.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadState {
    #[default]
    Reading,
    Ready,
    Failed(String),
}

/// A folder's diff as `g` shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalDiffData {
    /// The branch, or a short commit when detached.
    pub head: String,
    /// What the diff is measured from, as shown: `origin/main`, or `HEAD` for the
    /// uncommitted (with why, when a whole branch could not be).
    pub base: String,
    /// Some of it is not committed.
    pub dirty: bool,
    /// `viewed`: `Viewed` reviewed, `Dismissed` reviewed but changed since.
    pub files: Vec<crate::github::DiffFile>,
    /// Changed files beyond those read.
    pub more: u32,
}

/// `3 files +60 −28`, and whether some of it is not committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    pub dirty: bool,
}

/// How a pull request stopped being open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrEnd {
    Merged,
    Closed,
}

/// Where a session runs: its worktree, branch, repo and pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The worktree's top folder; the folder itself when it is not in a repo.
    pub root: PathBuf,
    /// `None`: detached, or not a repo.
    pub branch: Option<String>,
    /// The commit when detached, short.
    pub commit: Option<String>,
    /// Which of the project's repos the worktree belongs to.
    pub repo: Option<crate::github::RepoId>,
    /// The open pull request whose head is `branch`.
    pub pr: Option<crate::github::PrRef>,
    /// The folder is no longer there.
    pub gone: bool,
}

impl SessionInfo {
    /// The name a card shows: the user's own name, else the agent's terminal title,
    /// else the generated name.
    pub fn display_name(&self) -> &str {
        if self.user_named {
            return &self.name;
        }
        // An agent's title before it has a subject is only its program's name.
        self.title
            .as_deref()
            .filter(|t| !crate::autoname::generic_title(t))
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub projects: Vec<ProjectInfo>,
    pub sessions: Vec<SessionInfo>,
    /// The quick prompt's last choice, remembered across restarts.
    pub last_launch: Option<LaunchOptions>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_shows_its_own_name_its_agent_s_title_or_its_name() {
        let mut s = SessionInfo {
            id: crate::SessionId::new(),
            project: crate::ProjectId::new(),
            kind: SessionKind::Agent {
                harness: Harness::Claude,
            },
            name: "Fix Login".into(),
            status: crate::AgentStatus::Fresh,
            agent_session_id: None,
            title: Some("✳ Claude Code".into()),
            last_activity_ms: 0,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: "/w".into(),
            place: None,
        };
        assert_eq!(
            s.display_name(),
            "Fix Login",
            "the program's own name says nothing"
        );
        s.title = Some("✳ Login redirect".into());
        assert_eq!(s.display_name(), "✳ Login redirect");
        s.user_named = true;
        assert_eq!(s.display_name(), "Fix Login");
    }

    #[test]
    fn an_editor_is_gui_by_its_program_s_name() {
        assert!(gui_editor(Some("code --wait")));
        assert!(gui_editor(Some("/Applications/Cursor.app/bin/cursor")));
        assert!(!gui_editor(Some("nvim")));
        assert!(!gui_editor(Some("emacs -nw")));
        assert!(gui_editor(None), "none: the daemon looks for GUI ones");
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_says_the_file_it_opened_where_it_is() {
        let cwd = std::path::Path::new("/w/site");
        let tool = |args: &[&str]| SessionKind::Tool {
            program: "nvim".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        assert_eq!(
            tool(&["+42", "/w/site/src/a.rs"]).opens(cwd).as_deref(),
            Some("src/a.rs:42")
        );
        assert_eq!(tool(&["."]).opens(cwd), None);
        assert_eq!(tool(&[]).opens(cwd), None);
        assert_eq!(SessionKind::Shell.opens(cwd), None);
    }

    #[test]
    fn harness_ids_round_trip_and_all_is_ordered() {
        assert_eq!(
            Harness::ALL,
            [Harness::Claude, Harness::Codex, Harness::OpenCode]
        );
        for h in Harness::ALL {
            assert_eq!(Harness::from_id(h.id()), Some(h));
        }
        assert_eq!(Harness::OpenCode.id(), "opencode");
        assert_eq!(Harness::Codex.program(), "codex");
        assert_eq!(Harness::from_id("cursor"), None);
        assert_eq!(
            SessionKind::Agent {
                harness: Harness::Codex
            }
            .label(),
            "codex"
        );
    }

    #[test]
    fn only_claude_and_codex_take_an_effort_flag() {
        assert_eq!(
            Harness::Claude.efforts(),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(Harness::Codex.efforts(), ["low", "medium", "high"]);
        assert!(Harness::OpenCode.efforts().is_empty());
    }

    #[test]
    fn a_user_name_wins_over_the_title_and_the_title_over_the_generated_name() {
        let mut s = SessionInfo {
            id: SessionId::new(),
            project: ProjectId::new(),
            kind: SessionKind::Shell,
            name: "shell-1".into(),
            status: AgentStatus::Fresh,
            agent_session_id: None,
            title: None,
            last_activity_ms: 0,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: "/p".into(),
            place: None,
        };
        assert_eq!(s.display_name(), "shell-1");
        s.title = Some("Fix Login".into());
        assert_eq!(s.display_name(), "Fix Login");
        s.name = "login bug".into();
        s.user_named = true;
        assert_eq!(s.display_name(), "login bug");
    }
}
