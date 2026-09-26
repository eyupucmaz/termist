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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub projects: Vec<ProjectInfo>,
    pub sessions: Vec<SessionInfo>,
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
}
