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
    Agent { harness: Harness },
    Shell,
}

impl SessionKind {
    /// Short label shown on cards: the harness id, or "shell".
    pub fn label(&self) -> &'static str {
        match self {
            SessionKind::Agent { harness } => harness.id(),
            SessionKind::Shell => "shell",
        }
    }
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
        self.title.as_deref().unwrap_or(&self.name)
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
