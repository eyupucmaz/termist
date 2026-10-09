use crate::github::{GhState, PrDetail, PrDiff, PrRef, PrWrite, RepoId, RepoInfo, RepoPrs};
use crate::model::{
    DiffMode, GrepMatch, Harness, HarnessInfo, LaunchOptions, LocalDiffData, ModelInfo, ReadState,
    SessionInfo, SessionKind, StateSnapshot, TermColors, WorktreeInfo,
};
use crate::screen::{Pos, ScreenUpdate, Scroll};
use crate::{ProjectId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a ClientRequest/ServerEvent changes shape.
pub const PROTOCOL_VERSION: u32 = 14;

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
    /// `cwd` is a folder of the project, or a worktree of one of its repos; `None` is
    /// the project's folder.
    CreateSession {
        project: ProjectId,
        kind: SessionKind,
        cwd: Option<PathBuf>,
        prompt: Option<String>,
        /// What the card's name is made from, when not the prompt: the words typed
        /// into a preset's prompt, or the preset's name.
        title_from: Option<String>,
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
    /// What this client looks at: a project's pull requests, and maybe one of them,
    /// and maybe its diff. The daemon reads those more often; `None`, `None` is nothing.
    SetPrFocus {
        project: Option<ProjectId>,
        pr: Option<PrRef>,
        diff: bool,
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
    /// Marks a file of the pull request viewed on GitHub, or not.
    SetFileViewed {
        pr: PrRef,
        path: String,
        viewed: bool,
    },
    /// Writes to the pull request on GitHub; answered with `PrWritten` or
    /// `PrWriteFailed` carrying the same `ticket`.
    WritePr {
        pr: PrRef,
        ticket: u64,
        write: PrWrite,
    },
    /// A worktree on the pull request's branch: the one already there, else a new one.
    /// Answered with `WorktreeReady` or `WorktreeFailed`.
    OpenWorktree {
        pr: PrRef,
    },
    /// A new worktree of `repo` on `branch` (made from the repo's default branch when it
    /// is new). Answered with `WorktreeMade` or `WorktreeNotMade` carrying `ticket`.
    CreateWorktree {
        project: ProjectId,
        repo: RepoId,
        branch: String,
        ticket: u64,
    },
    /// Draw the worktree as a band even with no cards, or not.
    SetWorktreeShown {
        path: PathBuf,
        shown: bool,
    },
    /// Removes the worktree (the branch stays). Without `force` a worktree with
    /// uncommitted changes is answered with `RemoveRefused`.
    RemoveWorktree {
        path: PathBuf,
        force: bool,
    },
    /// This client looks at the diff of the folder at `path` (`None`: at none). Read at
    /// once, then again whenever the folder changes; answered with `LocalDiff`.
    SetLocalDiff {
        path: Option<PathBuf>,
        mode: DiffMode,
    },
    /// Marks `file` (a path in the diff of `worktree`) reviewed as it is now, or not.
    SetReviewed {
        worktree: PathBuf,
        file: String,
        reviewed: bool,
    },
    /// The folder, or a file in it at a line, in the user's editor: a window of its own
    /// for a GUI one, a card for one that runs in a terminal. `file` is relative to
    /// `folder`; `editor` is the user's choice (config, `$VISUAL`, `$EDITOR`), else the
    /// daemon looks for `code`, `cursor`, `zed`. Answered with `EditorFailed` when no
    /// editor can.
    OpenInEditor {
        project: ProjectId,
        folder: PathBuf,
        file: Option<String>,
        line: Option<u32>,
        editor: Option<String>,
    },
    /// The files of the repo the folder is in, for `f`; answered with `Files` or
    /// `FindFailed` carrying `ticket`.
    ListFiles {
        folder: PathBuf,
        ticket: u64,
    },
    /// `git grep` for `query` in the repo the folder is in; answered with `GrepResults`
    /// or `FindFailed`. Only a client's newest ticket is answered.
    Grep {
        folder: PathBuf,
        query: String,
        ticket: u64,
    },
    /// The text of a session's history from `from` to `to` (whole lines when `lines`);
    /// answered with `CopiedText`.
    CopyText {
        session: SessionId,
        from: Pos,
        to: Pos,
        lines: bool,
    },
    /// The next place `query` is in a session's history from `from`, up or down;
    /// answered with `Found`.
    Search {
        session: SessionId,
        query: String,
        from: Pos,
        backward: bool,
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
    /// A pull request's diff, to the clients that look at it; `diff` stays the last
    /// one read when `state` says the newest read failed.
    PrDiff {
        pr: PrRef,
        state: GhState,
        diff: Option<Box<PrDiff>>,
    },
    /// The worktree for an `OpenWorktree` of this client; `created` when termist made it.
    WorktreeReady {
        pr: PrRef,
        path: PathBuf,
        created: bool,
    },
    WorktreeFailed {
        pr: PrRef,
        message: String,
    },
    /// A project's worktrees, after every change; to every client.
    Worktrees {
        project: ProjectId,
        list: Vec<WorktreeInfo>,
    },
    /// The worktree a `CreateWorktree` of this client asked for; `note` says what was
    /// not as asked (made from the local branch when fetching failed).
    WorktreeMade {
        ticket: u64,
        path: PathBuf,
        branch: String,
        note: Option<String>,
    },
    WorktreeNotMade {
        ticket: u64,
        message: String,
    },
    /// The worktree has uncommitted changes in `files` files: removing it needs `force`.
    RemoveRefused {
        path: PathBuf,
        files: u32,
    },
    WorktreeRemoved {
        path: PathBuf,
    },
    /// A `RemoveWorktree` of this client did not happen.
    RemoveFailed {
        path: PathBuf,
        message: String,
    },
    /// The diff of a folder, to the clients that look at it; `diff` stays the last one
    /// read when `state` says the newest read failed.
    LocalDiff {
        path: PathBuf,
        mode: DiffMode,
        state: ReadState,
        diff: Option<Box<LocalDiffData>>,
    },
    /// An `OpenInEditor` of this client did not happen.
    EditorFailed {
        message: String,
    },
    /// The repo's files, relative to its `root`; `more` past those sent.
    Files {
        ticket: u64,
        root: PathBuf,
        files: Vec<String>,
        more: u32,
    },
    /// What `git grep` found; `more` when it found more than are sent.
    GrepResults {
        ticket: u64,
        root: PathBuf,
        matches: Vec<GrepMatch>,
        more: bool,
    },
    /// A `ListFiles` or `Grep` could not be done.
    FindFailed {
        ticket: u64,
        message: String,
    },
    CopiedText {
        session: SessionId,
        text: String,
    },
    /// Where `query` is next, its first and last cell, and which of how many it is.
    Found {
        session: SessionId,
        at: Option<(Pos, Pos)>,
        index: u32,
        total: u32,
    },
    /// A `WritePr` of this client went through.
    PrWritten {
        pr: PrRef,
        ticket: u64,
    },
    /// Something this client asked to change on GitHub did not happen: a `WritePr`
    /// (its ticket) or a viewed mark (`None`).
    PrWriteFailed {
        pr: PrRef,
        ticket: Option<u64>,
        message: String,
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
    fn the_protocol_is_14_since_the_presets() {
        assert_eq!(PROTOCOL_VERSION, 14);
    }
}
