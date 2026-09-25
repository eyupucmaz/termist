use crate::model::{Harness, SessionInfo, SessionKind, StateSnapshot};
use crate::screen::ScreenUpdate;
use crate::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a ClientRequest/ServerEvent changes shape.
pub const PROTOCOL_VERSION: u32 = 1;

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
