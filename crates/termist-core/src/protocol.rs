use crate::model::{Harness, HarnessInfo, LaunchOptions, SessionInfo, SessionKind, StateSnapshot};
use crate::screen::ScreenUpdate;
use crate::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a ClientRequest/ServerEvent changes shape.
pub const PROTOCOL_VERSION: u32 = 3;

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
    /// `model` and `effort` are passed to the CLI as flags; `None` passes none. A
    /// non-empty prompt goes into the prompt history, a model into the recent models.
    CreateSession {
        project: ProjectId,
        kind: SessionKind,
        prompt: Option<String>,
        model: Option<String>,
        effort: Option<String>,
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
    /// The name then stays: terminal titles no longer replace it.
    RenameSession {
        session: SessionId,
        name: String,
    },
    /// Stops a live session and hides its card; the record and the conversation stay.
    ArchiveSession {
        session: SessionId,
    },
    /// Shows the card again; resuming it is a separate `Resume`.
    UnarchiveSession {
        session: SessionId,
    },
    /// Hides a project's tab; its sessions keep running.
    CloseProject {
        project: ProjectId,
    },
    OpenProject {
        project: ProjectId,
    },
    /// Answered with `PromptHistory`.
    ListPromptHistory {
        limit: u32,
    },
    /// Remembers the quick prompt's choice; `State` carries it back.
    SetLastLaunch(LaunchOptions),
    /// Answered with `Models`.
    ListModels {
        harness: Harness,
    },
    /// Looks again for the agent CLIs that were missing; every client gets the
    /// result as `Harnesses`.
    RescanHarnesses,
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
    /// Earlier prompts, newest first.
    PromptHistory(Vec<String>),
    /// Models recently started with `harness`, most recent first.
    Models {
        harness: Harness,
        recent: Vec<String>,
    },
    Ack,
}
