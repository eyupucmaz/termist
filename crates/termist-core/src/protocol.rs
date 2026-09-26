use crate::model::{Harness, HarnessInfo, SessionInfo, SessionKind, StateSnapshot};
use crate::screen::ScreenUpdate;
use crate::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a ClientRequest/ServerEvent changes shape.
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// Must be the first frame on every connection.
    Hello {
        version: u32,
    },
    AddProject {
        path: PathBuf,
    },
    ListState,
    CreateSession {
        project: ProjectId,
        kind: SessionKind,
        prompt: Option<String>,
        cols: u16,
        rows: u16,
    },
    Attach {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    Detach {
        session: SessionId,
    },
    Input {
        session: SessionId,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    Resize {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    MarkSeen {
        session: SessionId,
    },
    KillSession {
        session: SessionId,
    },
    /// Relaunch a stopped (Exited or Disconnected) session in place, resuming the
    /// agent's own conversation when its id is known.
    Resume {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    /// Sent by `termist hook`; `payload_json` is the agent's hook stdin, verbatim.
    Hook {
        session: SessionId,
        harness: Harness,
        event: String,
        payload_json: String,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerEvent {
    Hello {
        version: u32,
        pid: u32,
    },
    State(StateSnapshot),
    /// Which agent CLIs the daemon can launch; sent after each `State` reply.
    Harnesses(Vec<HarnessInfo>),
    SessionUpdated(SessionInfo),
    SessionRemoved(SessionId),
    /// Pushed to attached clients; the first one after Attach carries every row.
    Screen {
        session: SessionId,
        update: ScreenUpdate,
    },
    Error {
        message: String,
    },
    Ack,
}
