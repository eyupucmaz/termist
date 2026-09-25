use crate::ids::{ProjectId, SessionId};
use crate::status::AgentStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Harness {
    Claude,
}

impl Harness {
    pub fn id(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
        }
    }

    pub fn from_id(s: &str) -> Option<Harness> {
        match s {
            "claude" => Some(Harness::Claude),
            _ => None,
        }
    }
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
