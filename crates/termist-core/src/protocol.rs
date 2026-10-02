use crate::github::{GhState, PrDetail, PrRef, RepoId, RepoInfo, RepoPrs};
use crate::model::{
    Harness, HarnessInfo, LaunchOptions, ModelInfo, SessionInfo, SessionKind, StateSnapshot,
    TermColors,
};
use crate::screen::{ScreenUpdate, Scroll};
use crate::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a ClientRequest/ServerEvent changes shape.
pub const PROTOCOL_VERSION: u32 = 7;

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
    /// Moves the session's view through its history. The view is the session's, not
    /// the client's; Attach and Input take it back to the live screen.
    Scroll {
        session: SessionId,
        scroll: Scroll,
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
    /// The colours the client draws agents with; every session answers colour queries
    /// with them from now on. The last client to send them wins.
    SetColors(TermColors),
    /// Whether termist reads GitHub at all (`[github] enabled`); the last client wins.
    SetGitHub {
        enabled: bool,
    },
    /// What this client looks at: a project's pull requests, and maybe one of them.
    /// The daemon reads those more often; `None`, `None` is nothing.
    SetPrFocus {
        project: Option<ProjectId>,
        pr: Option<PrRef>,
    },
    /// Answered with `Repos`, and again when the open counts come.
    ListRepos {
        project: ProjectId,
    },
    SetRepoVisible {
        repo: RepoId,
        visible: bool,
    },
    /// `None`: the account with the most access.
    SetRepoAccount {
        repo: RepoId,
        account: Option<String>,
    },
    /// Reads the project's pull requests again now.
    RefreshPrs {
        project: ProjectId,
    },
    /// The PR was opened as it was at `updated_at`.
    MarkPrSeen {
        pr: PrRef,
        updated_at: String,
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
    /// Earlier prompts, newest first.
    PromptHistory(Vec<String>),
    /// Models recently started with `harness`, most recent first, and the models its
    /// CLI offers (empty until the CLI has been asked; a second `Models` follows).
    Models {
        harness: Harness,
        recent: Vec<String>,
        catalog: Vec<ModelInfo>,
    },
    /// A project's pull requests, repo by repo, for its visible repos. `state` is the
    /// project-wide trouble (no gh, logged out); `discovered` counts every repo found.
    Prs {
        project: ProjectId,
        state: GhState,
        discovered: u32,
        repos: Vec<RepoPrs>,
    },
    /// Every repo found in a project, for the repos window, and the logged-in accounts.
    Repos {
        project: ProjectId,
        accounts: Vec<String>,
        repos: Vec<RepoInfo>,
    },
    /// The whole of one pull request; `detail` stays the last one read when `state`
    /// says the newest read failed.
    PrDetail {
        pr: PrRef,
        state: GhState,
        detail: Option<Box<PrDetail>>,
    },
    /// You were asked for a review since the last round.
    ReviewRequested {
        project: ProjectId,
        pr: PrRef,
        repo: String,
        title: String,
    },
    Ack,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_is_7_since_pull_requests() {
        assert_eq!(PROTOCOL_VERSION, 7);
    }
}
