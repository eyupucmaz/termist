use crate::launch::{LaunchRequest, Launcher};
use crate::session::{self, ClientId, SessionCmd, SessionNote};
use crate::store::{PROMPT_HISTORY_MAX, Store, StoredSession};
use crate::transcript::TranscriptTail;
use crate::{claude, codex, opencode};
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, LaunchOptions, ProjectId, ProjectInfo,
    ServerEvent, SessionId, SessionInfo, SessionKind, Signal, StateSnapshot, now_ms,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

/// How often a session's PTY activity is broadcast to clients as `SessionUpdated`.
const ACTIVITY_BROADCAST: Duration = Duration::from_secs(5);

/// A running Claude card whose title has been idle this long, with no hook in between,
/// was cancelled before its answer started (Claude sends no hook for that).
const IDLE_TITLE_CANCEL: Duration = Duration::from_millis(1500);

/// A rescan for missing CLIs can start a login shell; one per this window is enough.
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);

/// The agent CLIs a rescan found, with the program to launch for each.
#[derive(Debug)]
pub struct Rescanned(pub Vec<(Harness, PathBuf)>);

pub enum Msg {
    Connected {
        client: ClientId,
        out: UnboundedSender<ServerEvent>,
    },
    Disconnected(ClientId),
    Request {
        client: ClientId,
        req: ClientRequest,
    },
}

struct Session {
    info: SessionInfo,
    /// `None` for a session loaded from the store that has not been resumed yet.
    cmd: Option<UnboundedSender<SessionCmd>>,
    transcript: Option<TranscriptTail>,
    /// Last time an `Activity` note was broadcast; throttles `SessionUpdated`.
    activity_broadcast: Option<std::time::Instant>,
    /// The agent's conversation exists, so Resume may pass its id. Claude's id is
    /// assigned up front, but Claude only creates the conversation with the first prompt.
    resumable: bool,
    /// OpenCode subagent session ids seen for this card; their events are ignored.
    opencode_children: HashSet<String>,
    /// Claude: since when the title has shown idle while the card is running.
    idle_title_since: Option<std::time::Instant>,
}

impl Session {
    fn new(
        info: SessionInfo,
        cmd: Option<UnboundedSender<SessionCmd>>,
        resumable: bool,
    ) -> Session {
        Session {
            info,
            cmd,
            transcript: None,
            activity_broadcast: None,
            resumable,
            opencode_children: HashSet::new(),
            idle_title_since: None,
        }
    }
}

pub struct Registry {
    launcher: Launcher,
    harnesses: Vec<HarnessInfo>,
    store: Store,
    projects: Vec<ProjectInfo>,
    sessions: Vec<Session>,
    /// The quick prompt's last choice, as the store has it.
    last_launch: Option<LaunchOptions>,
    clients: HashMap<ClientId, UnboundedSender<ServerEvent>>,
    notes: UnboundedSender<SessionNote>,
    shutdown: Option<oneshot::Sender<()>>,
    created: u32,
    /// A rescan runs on a blocking thread and reports back through this channel;
    /// `run` takes the receiving end.
    rescans: UnboundedSender<Rescanned>,
    rescans_rx: Option<UnboundedReceiver<Rescanned>>,
    rescanning: bool,
    /// When the last rescan started.
    last_rescan: Option<std::time::Instant>,
    /// Looks a CLI up (PATH, then the login shell).
    find_program: fn(&str) -> Option<PathBuf>,
}

impl Registry {
    pub fn new(
        launcher: Launcher,
        harnesses: Vec<HarnessInfo>,
        store: Store,
        notes: UnboundedSender<SessionNote>,
        shutdown: oneshot::Sender<()>,
    ) -> Registry {
        let (projects, sessions) = store.load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not load stored sessions");
            (vec![], vec![])
        });
        // Names are `<label>-<n>`: go on from the highest n, so none repeats.
        let created = sessions
            .iter()
            .filter_map(|s| s.info.name.rsplit_once('-')?.1.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        let last_launch = store.last_launch();
        let (rescans, rescans_rx) = tokio::sync::mpsc::unbounded_channel();
        Registry {
            launcher,
            harnesses,
            store,
            projects,
            sessions: sessions
                .into_iter()
                .map(|StoredSession { info, resumable }| Session::new(info, None, resumable))
                .collect(),
            last_launch,
            clients: HashMap::new(),
            notes,
            shutdown: Some(shutdown),
            created,
            rescans,
            rescans_rx: Some(rescans_rx),
            rescanning: false,
            last_rescan: None,
            find_program: crate::resolve::find_program,
        }
    }

    /// Write-through: mirrors a session's current info into the store.
    fn persist(&self, id: SessionId) {
        if let Some(s) = self.session(id)
            && let Err(e) = self.store.upsert_session(&s.info, s.resumable)
        {
            tracing::warn!(session = %id, error = %e, "could not store session");
        }
    }

    fn state(&self) -> StateSnapshot {
        StateSnapshot {
            projects: self.projects.clone(),
            sessions: self.sessions.iter().map(|s| s.info.clone()).collect(),
            last_launch: self.last_launch.clone(),
        }
    }

    fn send(&self, client: ClientId, event: ServerEvent) {
        if let Some(out) = self.clients.get(&client) {
            let _ = out.send(event);
        }
    }

    fn broadcast(&self, event: ServerEvent) {
        for out in self.clients.values() {
            let _ = out.send(event.clone());
        }
    }

    /// Tells `client` why its request failed, if it did.
    fn report(&self, client: ClientId, result: anyhow::Result<()>) {
        if let Err(e) = result {
            self.send(
                client,
                ServerEvent::Error {
                    message: format!("{e:#}"),
                },
            );
        }
    }

    /// Stores and broadcasts a session's changed info.
    fn updated(&self, id: SessionId) {
        if let Some(s) = self.session(id) {
            self.persist(id);
            self.broadcast(ServerEvent::SessionUpdated(s.info.clone()));
        }
    }

    fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.info.id == id)
    }

    fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.info.id == id)
    }

    /// Applies a status signal; broadcasts only when the status actually changed.
    fn signal(&mut self, id: SessionId, signal: Signal) {
        let Some(s) = self.session_mut(id) else {
            return;
        };
        let next = s.info.status.apply(signal);
        if next == s.info.status {
            return;
        }
        s.info.status = next;
        s.info.last_activity_ms = now_ms();
        let info = s.info.clone();
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Connected { client, out } => {
                self.clients.insert(client, out);
            }
            Msg::Disconnected(client) => {
                self.clients.remove(&client);
                for s in &self.sessions {
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Detach { client });
                    }
                }
            }
            Msg::Request { client, req } => self.request(client, req),
        }
    }

    pub fn note(&mut self, note: SessionNote) {
        match note {
            SessionNote::Title(id, title) => {
                // Some agents animate their title; only a real change is stored and sent.
                if let Some(s) = self.session_mut(id)
                    && s.info.title != title
                {
                    let idle = s.info.kind
                        == (SessionKind::Agent {
                            harness: Harness::Claude,
                        })
                        && s.info.status == AgentStatus::Running
                        && title.as_deref().is_some_and(claude::title_is_idle);
                    s.idle_title_since = if idle {
                        s.idle_title_since
                            .or_else(|| Some(std::time::Instant::now()))
                    } else {
                        None
                    };
                    s.info.title = title;
                    let info = s.info.clone();
                    self.persist(id);
                    self.broadcast(ServerEvent::SessionUpdated(info));
                }
            }
            SessionNote::Activity(id) => {
                let due = self.session_mut(id).and_then(|s| {
                    s.info.last_activity_ms = now_ms();
                    let due = s
                        .activity_broadcast
                        .is_none_or(|t| t.elapsed() >= ACTIVITY_BROADCAST);
                    if due {
                        s.activity_broadcast = Some(std::time::Instant::now());
                    }
                    due.then(|| s.info.clone())
                });
                if let Some(info) = due {
                    self.broadcast(ServerEvent::SessionUpdated(info));
                }
            }
            SessionNote::Exited(id, code) => self.signal(id, Signal::ProcessExited { code }),
        }
    }

    fn request(&mut self, client: ClientId, req: ClientRequest) {
        match req {
            ClientRequest::Hello { .. } => self.send(
                client,
                ServerEvent::Error {
                    message: "duplicate hello".into(),
                },
            ),
            ClientRequest::ListState => {
                self.send(client, ServerEvent::State(self.state()));
                self.send(client, ServerEvent::Harnesses(self.harnesses.clone()));
            }
            ClientRequest::AddProject { path } => match self.add_project(path) {
                Ok(()) => self.broadcast(ServerEvent::State(self.state())),
                Err(e) => self.send(
                    client,
                    ServerEvent::Error {
                        message: format!("{e:#}"),
                    },
                ),
            },
            ClientRequest::CreateSession {
                project,
                kind,
                prompt,
                model,
                effort,
                cols,
                rows,
            } => {
                if let Err(e) =
                    self.create_session(project, kind, prompt, model, effort, (cols, rows))
                {
                    self.send(
                        client,
                        ServerEvent::Error {
                            message: format!("{e:#}"),
                        },
                    );
                }
            }
            ClientRequest::Attach {
                session,
                cols,
                rows,
            } => {
                if let (Some(s), Some(out)) = (self.session(session), self.clients.get(&client))
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Resize { cols, rows });
                    let _ = cmd.send(SessionCmd::Attach {
                        client,
                        out: out.clone(),
                    });
                }
            }
            ClientRequest::Detach { session } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Detach { client });
                }
            }
            ClientRequest::Input { session, data } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Input(data));
                }
                self.signal(session, Signal::UserTyped);
            }
            ClientRequest::Resize {
                session,
                cols,
                rows,
            } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Resize { cols, rows });
                }
            }
            ClientRequest::MarkSeen { session } => self.signal(session, Signal::Seen),
            ClientRequest::KillSession { session } => {
                if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session) {
                    let s = self.sessions.remove(pos);
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Kill);
                    }
                    if let Err(e) = self.store.delete_session(session) {
                        tracing::warn!(session = %session, error = %e, "could not delete session");
                    }
                    self.broadcast(ServerEvent::SessionRemoved(session));
                }
            }
            ClientRequest::Hook {
                session,
                harness,
                event,
                payload_json,
            } => {
                let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
                self.hook(session, harness, &event, &payload);
                self.send(client, ServerEvent::Ack);
            }
            ClientRequest::Resume {
                session,
                cols,
                rows,
            } => {
                if let Err(e) = self.resume(session, cols, rows) {
                    self.send(
                        client,
                        ServerEvent::Error {
                            message: format!("{e:#}"),
                        },
                    );
                }
            }
            ClientRequest::RenameSession { session, name } => {
                let result = self.rename(session, &name);
                self.report(client, result);
            }
            ClientRequest::ArchiveSession { session } => {
                let result = self.set_archived(session, true);
                self.report(client, result);
            }
            ClientRequest::UnarchiveSession { session } => {
                let result = self.set_archived(session, false);
                self.report(client, result);
            }
            ClientRequest::CloseProject { project } => {
                self.set_project_open(client, project, false)
            }
            ClientRequest::OpenProject { project } => self.set_project_open(client, project, true),
            ClientRequest::ListPromptHistory { limit } => {
                let limit = (limit as usize).min(PROMPT_HISTORY_MAX);
                let history = self.store.prompt_history(limit).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "could not read the prompt history");
                    vec![]
                });
                self.send(client, ServerEvent::PromptHistory(history));
            }
            ClientRequest::SetLastLaunch(launch) => {
                if let Err(e) = self.store.set_last_launch(&launch) {
                    tracing::warn!(error = %e, "could not store the last launch");
                }
                self.last_launch = Some(launch);
            }
            ClientRequest::ListModels { harness } => {
                let recent = self.store.recent_models(harness);
                self.send(client, ServerEvent::Models { harness, recent });
            }
            ClientRequest::RescanHarnesses => self.rescan(std::time::Instant::now()),
            ClientRequest::Shutdown => {
                for s in &self.sessions {
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Kill);
                    }
                }
                self.send(client, ServerEvent::Ack);
                if let Some(tx) = self.shutdown.take() {
                    let _ = tx.send(());
                }
            }
        }
    }

    fn hook(&mut self, id: SessionId, harness: Harness, event: &str, payload: &Value) {
        // Any hook means Claude is still talking to us: the title alone decides nothing.
        if let Some(s) = self.session_mut(id) {
            s.idle_title_since = None;
        }
        tracing::debug!(
            session = %id,
            harness = harness.id(),
            event,
            session_id = payload.get("session_id").and_then(serde_json::Value::as_str),
            source = payload.get("source").and_then(serde_json::Value::as_str),
            transcript_path = payload.get("transcript_path").and_then(serde_json::Value::as_str),
            hook_event_name = payload
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str),
            turn_id = payload.get("turn_id").and_then(serde_json::Value::as_str),
            opencode_session_id = payload
                .pointer("/properties/sessionID")
                .and_then(serde_json::Value::as_str),
            opencode_info_id = payload
                .pointer("/properties/info/id")
                .and_then(serde_json::Value::as_str),
            opencode_info_parent_id = payload
                .pointer("/properties/info/parentID")
                .and_then(serde_json::Value::as_str),
            "received hook"
        );
        let signal = match harness {
            Harness::Claude => {
                // `/clear` starts a new conversation, which exists only with its first prompt.
                let cleared = event == "SessionStart"
                    && payload.get("source").and_then(Value::as_str) == Some("clear")
                    && payload.get("session_id").and_then(Value::as_str)
                        != self
                            .session(id)
                            .and_then(|s| s.info.agent_session_id.as_deref());
                // Claude's id is known before its conversation exists: not a sign of one.
                self.capture_session_start(id, event, payload);
                if cleared {
                    self.forget_conversation(id);
                }
                if let Some(path) = payload.get("transcript_path").and_then(Value::as_str) {
                    self.watch_transcript(id, Path::new(path));
                }
                if event == "UserPromptSubmit" {
                    self.skip_transcript_so_far(id);
                }
                claude::signal_for(event, payload)
            }
            Harness::Codex => {
                if self.capture_codex_session_start(id, event, payload) {
                    self.mark_resumable(id);
                }
                codex::signal_for(event, payload)
            }
            Harness::OpenCode => self.opencode_hook(id, event, payload),
        };
        if signal == Some(Signal::PromptSubmitted) {
            self.mark_resumable(id);
        }
        if let Some(signal) = signal {
            self.signal(id, signal);
        }
    }

    /// Takes the agent's id from a `SessionStart` payload; true when there was one.
    fn capture_session_start(&mut self, id: SessionId, event: &str, payload: &Value) -> bool {
        match payload.get("session_id").and_then(Value::as_str) {
            Some(sid) if event == "SessionStart" => {
                self.set_agent_session_id(id, sid);
                true
            }
            _ => false,
        }
    }

    /// Codex only: like `capture_session_start`, but a `SessionStart` for a different
    /// id that arrives mid-turn (Running or NeedsFeedback) is ignored. Codex's
    /// internal/sub-agent sessions inherit our `-c` hook flags and fire their own
    /// SessionStart while the user's turn is still running; a genuine switch to a new
    /// Codex conversation can only happen between turns, so this can't mistake one
    /// for the other. The SessionStart itself carries no status signal for Codex.
    fn capture_codex_session_start(&mut self, id: SessionId, event: &str, payload: &Value) -> bool {
        let Some(sid) = payload.get("session_id").and_then(Value::as_str) else {
            return false;
        };
        if event != "SessionStart" {
            return false;
        }
        if let Some(s) = self.session(id)
            && matches!(
                s.info.status,
                AgentStatus::Running | AgentStatus::NeedsFeedback
            )
            && s.info.agent_session_id.as_deref() != Some(sid)
        {
            tracing::debug!(
                session = %id,
                session_id = sid,
                "ignored mid-turn Codex SessionStart for another session"
            );
            return false;
        }
        self.set_agent_session_id(id, sid);
        true
    }

    /// The card follows the OpenCode session the user is in: a session created
    /// without a parent (`/new`) or a message in another session (switching) moves
    /// it there. Subagent sessions (created with a parent) never move the card.
    fn opencode_hook(&mut self, id: SessionId, event: &str, payload: &Value) -> Option<Signal> {
        let sid = opencode::event_session(payload).map(str::to_string);
        let s = self.session_mut(id)?;
        if event == "session.created" {
            match sid {
                Some(sid) if opencode::is_child_session(payload) => {
                    s.opencode_children.insert(sid);
                }
                Some(sid) => self.adopt_agent_session(id, &sid),
                None => {}
            }
            return None;
        }
        if let Some(sid) = sid {
            if s.opencode_children.contains(&sid) {
                return None;
            }
            if event == "chat.message" {
                self.adopt_agent_session(id, &sid);
            }
        }
        opencode::signal_for(event, payload)
    }

    /// An id the agent reported for a conversation it has: resume can use it.
    fn adopt_agent_session(&mut self, id: SessionId, sid: &str) {
        self.set_agent_session_id(id, sid);
        self.mark_resumable(id);
    }

    fn mark_resumable(&mut self, id: SessionId) {
        if let Some(s) = self.session_mut(id)
            && !s.resumable
        {
            s.resumable = true;
            self.persist(id);
        }
    }

    /// Resume starts the agent fresh until the conversation exists again.
    fn forget_conversation(&mut self, id: SessionId) {
        if let Some(s) = self.session_mut(id)
            && s.resumable
        {
            s.resumable = false;
            self.persist(id);
        }
    }

    fn set_agent_session_id(&mut self, id: SessionId, sid: &str) {
        if let Some(s) = self.session_mut(id)
            && s.info.agent_session_id.as_deref() != Some(sid)
        {
            s.info.agent_session_id = Some(sid.to_string());
            let info = s.info.clone();
            self.persist(id);
            self.broadcast(ServerEvent::SessionUpdated(info));
        }
    }

    fn watch_transcript(&mut self, id: SessionId, path: &Path) {
        if let Some(s) = self.session_mut(id)
            && s.transcript.as_ref().is_none_or(|t| t.path() != path)
        {
            s.transcript = Some(TranscriptTail::new(path.to_path_buf()));
        }
    }

    /// A new turn starts: an interrupt line already in the transcript (the previous
    /// turn's, not read yet because polling only runs mid-turn) must not cancel it.
    fn skip_transcript_so_far(&mut self, id: SessionId) {
        if let Some(t) = self.session_mut(id).and_then(|s| s.transcript.as_mut()) {
            t.poll();
        }
    }

    /// Claude cards still running with an idle title and no hook for `IDLE_TITLE_CANCEL`:
    /// the turn was cancelled before its answer started.
    pub fn poll_idle_titles(&mut self, now: std::time::Instant) {
        let cancelled: Vec<SessionId> = self
            .sessions
            .iter_mut()
            .filter(|s| s.info.status == AgentStatus::Running)
            .filter_map(|s| {
                let since = s.idle_title_since?;
                (now.duration_since(since) >= IDLE_TITLE_CANCEL).then(|| {
                    s.idle_title_since = None;
                    s.info.id
                })
            })
            .collect();
        for id in cancelled {
            self.signal(id, Signal::Cancelled);
        }
    }

    /// Claude sessions that are mid-turn and whose transcript shows an interrupt.
    pub fn poll_transcripts(&mut self) {
        let mut cancelled = Vec::new();
        for s in &mut self.sessions {
            if matches!(
                s.info.status,
                AgentStatus::Running | AgentStatus::NeedsFeedback
            ) && let Some(t) = s.transcript.as_mut()
                && t.poll()
            {
                cancelled.push(s.info.id);
            }
        }
        for id in cancelled {
            self.signal(id, Signal::Cancelled);
        }
    }

    /// A known path reopens its project; an unknown one is added.
    fn add_project(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let path = std::fs::canonicalize(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        if let Some(p) = self.projects.iter_mut().find(|p| p.path == path) {
            if !p.open {
                p.open = true;
                if let Err(e) = self.store.upsert_project(p) {
                    tracing::warn!(error = %e, "could not store project");
                }
            }
            return Ok(());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.projects.push(ProjectInfo {
            id: ProjectId::new(),
            name,
            path,
            open: true,
        });
        if let Err(e) = self.store.upsert_project(self.projects.last().unwrap()) {
            tracing::warn!(error = %e, "could not store project");
        }
        Ok(())
    }

    /// Looks again, on a blocking thread, for the CLIs that were missing (one rescan at
    /// a time, and none within `RESCAN_INTERVAL` of the last one's start). OpenCode also
    /// needs its plugin, so it is written again when found.
    fn rescan(&mut self, now: std::time::Instant) {
        let missing: Vec<Harness> = self
            .harnesses
            .iter()
            .filter(|h| !h.available)
            .map(|h| h.harness)
            .collect();
        let too_soon = self
            .last_rescan
            .is_some_and(|t| now.saturating_duration_since(t) < RESCAN_INTERVAL);
        if self.rescanning || too_soon || missing.is_empty() {
            return;
        }
        self.rescanning = true;
        self.last_rescan = Some(now);
        let (tx, find) = (self.rescans.clone(), self.find_program);
        let opencode_dir = self.launcher.opencode_config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let found = missing
                .into_iter()
                .filter_map(|harness| {
                    let program = find(harness.program())?;
                    if harness == Harness::OpenCode
                        && let Err(e) = opencode::write_plugin(&opencode_dir)
                    {
                        tracing::warn!(error = %e, "could not write the OpenCode plugin");
                        return None;
                    }
                    Some((harness, program))
                })
                .collect();
            let _ = tx.send(Rescanned(found));
        });
    }

    /// A rescan finished: newly found CLIs become available to every client.
    pub fn rescanned(&mut self, Rescanned(found): Rescanned) {
        self.rescanning = false;
        if found.is_empty() {
            return;
        }
        for (harness, program) in found {
            self.launcher
                .programs
                .set(harness, program.display().to_string());
            if let Some(h) = self.harnesses.iter_mut().find(|h| h.harness == harness) {
                h.available = true;
            }
        }
        tracing::info!(harnesses = ?self.harnesses, "agent CLIs after a rescan");
        self.broadcast(ServerEvent::Harnesses(self.harnesses.clone()));
    }

    /// Closing hides the tab; the sessions keep running. Every client gets the new state.
    fn set_project_open(&mut self, client: ClientId, project: ProjectId, open: bool) {
        let Some(p) = self.projects.iter_mut().find(|p| p.id == project) else {
            self.report(client, Err(anyhow::anyhow!("unknown project")));
            return;
        };
        p.open = open;
        if let Err(e) = self.store.upsert_project(p) {
            tracing::warn!(error = %e, "could not store project");
        }
        self.broadcast(ServerEvent::State(self.state()));
    }

    /// The new name sticks: terminal titles no longer replace it.
    fn rename(&mut self, id: SessionId, name: &str) -> anyhow::Result<()> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            bail!("a session name cannot be empty");
        }
        let Some(s) = self.session_mut(id) else {
            bail!("unknown session")
        };
        s.info.name = name;
        s.info.user_named = true;
        self.updated(id);
        Ok(())
    }

    /// Archiving stops a live session; the record (and the agent's conversation) stays.
    fn set_archived(&mut self, id: SessionId, archived: bool) -> anyhow::Result<()> {
        let Some(s) = self.session_mut(id) else {
            bail!("unknown session")
        };
        if archived
            && s.info.status.is_live()
            && let Some(cmd) = &s.cmd
        {
            let _ = cmd.send(SessionCmd::Kill);
        }
        s.info.archived = archived;
        self.updated(id);
        Ok(())
    }

    fn create_session(
        &mut self,
        project: ProjectId,
        kind: SessionKind,
        prompt: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        (cols, rows): (u16, u16),
    ) -> anyhow::Result<()> {
        let Some(proj) = self.projects.iter().find(|p| p.id == project) else {
            bail!("unknown project")
        };
        // A shell has no model or effort; an agent only takes an effort its CLI knows.
        let (model, effort) = match &kind {
            SessionKind::Shell => (None, None),
            SessionKind::Agent { harness } => {
                if let Some(e) = &effort
                    && !harness.efforts().contains(&e.as_str())
                {
                    bail!("{} has no effort level {e:?}", harness.id());
                }
                let model = model
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty());
                (model, effort)
            }
        };
        let id = SessionId::new();
        let launch = self.launcher.launch(LaunchRequest {
            id,
            kind: &kind,
            prompt: prompt.as_deref(),
            model: model.as_deref(),
            effort: effort.as_deref(),
            cwd: &proj.path,
            cols,
            rows,
            resume: None,
        });
        let program = launch.spec.program.clone();
        let cmd = session::spawn(launch.spec, self.notes.clone())
            .with_context(|| format!("could not start {} ({program})", kind.label()))?;
        self.remember_launch(&kind, prompt.as_deref(), model.as_deref());
        self.created += 1;
        let info = SessionInfo {
            id,
            project,
            name: format!("{}-{}", kind.label(), self.created),
            kind,
            status: AgentStatus::Fresh,
            agent_session_id: launch.agent_session_id,
            title: None,
            last_activity_ms: now_ms(),
            model,
            effort,
            user_named: false,
            archived: false,
        };
        self.sessions
            .push(Session::new(info.clone(), Some(cmd), false));
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
        Ok(())
    }

    /// A started session's prompt goes into the history, its model into the recent models.
    fn remember_launch(&self, kind: &SessionKind, prompt: Option<&str>, model: Option<&str>) {
        if let Some(p) = prompt.filter(|p| !p.trim().is_empty())
            && let Err(e) = self.store.add_prompt(p)
        {
            tracing::warn!(error = %e, "could not store the prompt");
        }
        if let (SessionKind::Agent { harness }, Some(m)) = (kind, model)
            && let Err(e) = self.store.add_recent_model(*harness, m)
        {
            tracing::warn!(error = %e, "could not store the model");
        }
    }

    fn resume(&mut self, id: SessionId, cols: u16, rows: u16) -> anyhow::Result<()> {
        let Some(pos) = self.sessions.iter().position(|s| s.info.id == id) else {
            bail!("unknown session")
        };
        let info = &self.sessions[pos].info;
        if info.status.is_live() {
            bail!("{} is still running", info.name);
        }
        let Some(project) = self.projects.iter().find(|p| p.id == info.project) else {
            bail!("the project of {} is gone", info.name)
        };
        let kind = info.kind.clone();
        let (model, effort) = (info.model.clone(), info.effort.clone());
        // Without a conversation to resume (no prompt yet), start the agent fresh.
        let resume = if self.sessions[pos].resumable {
            info.agent_session_id.clone()
        } else {
            None
        };
        let launch = self.launcher.launch(LaunchRequest {
            id,
            kind: &kind,
            prompt: None,
            model: model.as_deref(),
            effort: effort.as_deref(),
            cwd: &project.path,
            cols,
            rows,
            resume: resume.as_deref(),
        });
        let program = launch.spec.program.clone();
        let cmd = session::spawn(launch.spec, self.notes.clone())
            .with_context(|| format!("could not start {} ({program})", kind.label()))?;
        let s = &mut self.sessions[pos];
        s.cmd = Some(cmd); // dropping the old sender ends the old session task
        s.transcript = None;
        s.activity_broadcast = None;
        s.info.title = None; // the new process sets its own
        s.info.status = AgentStatus::Fresh;
        s.resumable = resume.is_some();
        s.info.agent_session_id = launch.agent_session_id;
        s.info.last_activity_ms = now_ms();
        let info = s.info.clone();
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
        Ok(())
    }
}

pub async fn run(
    mut reg: Registry,
    mut rx: UnboundedReceiver<Msg>,
    mut notes: UnboundedReceiver<SessionNote>,
) {
    let mut transcripts = tokio::time::interval(Duration::from_millis(500));
    transcripts.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut rescans = reg.rescans_rx.take().expect("a registry runs once");
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(msg) => reg.handle(msg),
                None => break,
            },
            Some(note) = notes.recv() => reg.note(note),
            Some(found) = rescans.recv() => reg.rescanned(found),
            _ = transcripts.tick() => {
                reg.poll_transcripts();
                reg.poll_idle_titles(std::time::Instant::now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{DaemonConfig, HarnessPrograms};
    use crate::store::Store;
    use tokio::sync::mpsc::unbounded_channel;

    fn registry_with(project: &ProjectInfo, sessions: &[SessionInfo]) -> Registry {
        let store = Store::open_in_memory();
        store.upsert_project(project).unwrap();
        for s in sessions {
            store.upsert_session(s, false).unwrap();
        }
        let launcher = Launcher {
            config: DaemonConfig::default(),
            programs: HarnessPrograms {
                claude: "claude".into(),
                codex: "codex".into(),
                opencode: "opencode".into(),
            },
            exe: PathBuf::from("/t/termist"),
            claude_settings: PathBuf::from("/t/claude-hooks.json"),
            runtime_dir: PathBuf::from("/t/run"),
            termist_home: None,
            opencode_config_dir: PathBuf::from("/t/opencode"),
        };
        let (notes, _notes_rx) = unbounded_channel();
        let (stop, _stop_rx) = oneshot::channel();
        Registry::new(launcher, vec![], store, notes, stop)
    }

    fn project() -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: std::env::temp_dir(),
            open: true,
        }
    }

    fn stored(p: &ProjectInfo, name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project: p.id,
            kind: SessionKind::Shell,
            name: name.into(),
            status: AgentStatus::Disconnected,
            agent_session_id: None,
            title: None,
            last_activity_ms: 1,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
        }
    }

    fn connect(reg: &mut Registry) -> UnboundedReceiver<ServerEvent> {
        let (out, rx) = unbounded_channel();
        reg.handle(Msg::Connected {
            client: ClientId(1),
            out,
        });
        rx
    }

    #[test]
    fn names_keep_counting_from_the_highest_stored_number() {
        let p = project();
        let names = [
            "claude-1", "claude-3", "shell-2", "my-notes", "codex-x", "odd",
        ];
        let sessions: Vec<_> = names.iter().map(|n| stored(&p, n)).collect();
        assert_eq!(registry_with(&p, &sessions).created, 3);
        assert_eq!(registry_with(&p, &[]).created, 0);
    }

    #[test]
    fn a_repeated_title_is_neither_stored_nor_broadcast_again() {
        let p = project();
        let s = stored(&p, "codex-1");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        let mut rx = connect(&mut reg);
        for _ in 0..2 {
            reg.note(SessionNote::Title(s.id, Some("Working".into())));
        }
        let mut updates = vec![];
        while let Ok(ev) = rx.try_recv() {
            updates.push(ev);
        }
        assert_eq!(updates.len(), 1, "{updates:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resume_starts_with_no_title_and_a_fresh_activity_window() {
        let p = project();
        let mut s = stored(&p, "shell-1");
        s.title = Some("old title".into());
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().activity_broadcast = Some(std::time::Instant::now());
        let _rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::Resume {
                session: s.id,
                cols: 80,
                rows: 24,
            },
        });
        let back = reg.session(s.id).unwrap();
        assert_eq!(back.info.status, AgentStatus::Fresh);
        assert_eq!(back.info.title, None);
        assert_eq!(back.activity_broadcast, None);
        assert_eq!(
            reg.store.load().unwrap().1[0].info.title,
            None,
            "stored too"
        );
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::KillSession { session: s.id },
        });
    }

    #[test]
    fn activity_is_broadcast_at_most_once_per_window() {
        let p = project();
        let s = stored(&p, "shell-1");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        let mut rx = connect(&mut reg);
        reg.note(SessionNote::Activity(s.id));
        reg.note(SessionNote::Activity(s.id));
        let mut updates = vec![];
        while let Ok(ev) = rx.try_recv() {
            updates.push(ev);
        }
        assert_eq!(updates.len(), 1, "{updates:?}");
        match &updates[0] {
            ServerEvent::SessionUpdated(info) => assert!(info.last_activity_ms > 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_effort_the_cli_does_not_know_is_refused_before_anything_starts() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let agent = |harness| SessionKind::Agent { harness };
        for (harness, effort) in [(Harness::Claude, "turbo"), (Harness::OpenCode, "high")] {
            let err = reg
                .create_session(
                    p.id,
                    agent(harness),
                    None,
                    None,
                    Some(effort.into()),
                    (80, 24),
                )
                .unwrap_err();
            assert!(err.to_string().contains("no effort level"), "{err}");
        }
        assert!(reg.sessions.is_empty());
    }

    // An archived card stays archived whatever its hooks say; only its status moves.
    #[test]
    fn hooks_move_an_archived_sessions_status_but_keep_it_archived() {
        let p = project();
        let mut s = stored(&p, "claude-1");
        s.kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        s.archived = true;
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().info.status = AgentStatus::Running;
        let mut rx = connect(&mut reg);
        reg.hook(s.id, Harness::Claude, "Stop", &Value::Null);
        match rx.try_recv() {
            Ok(ServerEvent::SessionUpdated(info)) => {
                assert_eq!(info.status, AgentStatus::Unseen);
                assert!(info.archived);
            }
            other => panic!("{other:?}"),
        }
        assert!(reg.store.load().unwrap().1[0].info.archived, "stored too");
    }

    fn claude(p: &ProjectInfo, agent_id: &str) -> SessionInfo {
        let mut s = stored(p, "claude-1");
        s.kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        s.agent_session_id = Some(agent_id.into());
        s
    }

    #[test]
    fn a_cleared_claude_conversation_is_not_resumable_until_its_first_prompt() {
        let p = project();
        let s = claude(&p, "old");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().resumable = true;
        let start =
            |sid: &str, source: &str| serde_json::json!({ "session_id": sid, "source": source });
        reg.hook(
            s.id,
            Harness::Claude,
            "SessionStart",
            &start("old", "resume"),
        );
        assert!(
            reg.session(s.id).unwrap().resumable,
            "a resumed conversation is kept"
        );
        reg.hook(
            s.id,
            Harness::Claude,
            "SessionStart",
            &start("new", "clear"),
        );
        let cleared = reg.session(s.id).unwrap();
        assert_eq!(cleared.info.agent_session_id.as_deref(), Some("new"));
        assert!(!cleared.resumable);
        assert!(!reg.store.load().unwrap().1[0].resumable, "stored too");
        reg.hook(s.id, Harness::Claude, "UserPromptSubmit", &Value::Null);
        assert!(reg.session(s.id).unwrap().resumable);
    }

    fn missing_codex_and_opencode(reg: &mut Registry) {
        reg.harnesses = Harness::ALL
            .into_iter()
            .map(|harness| HarnessInfo {
                harness,
                available: harness == Harness::Claude,
            })
            .collect();
    }

    #[tokio::test]
    async fn a_rescan_looks_only_for_the_missing_clis_and_tells_every_client() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let tmp = tempfile::tempdir().unwrap();
        reg.launcher.opencode_config_dir = tmp.path().join("opencode");
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |name| Some(PathBuf::from(format!("/new/{name}")));
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        let mut results = reg.rescans_rx.take().unwrap();
        let found = results.recv().await.unwrap();
        assert!(results.try_recv().is_err(), "one rescan at a time");
        reg.rescanned(found);
        match rx.try_recv() {
            Ok(ServerEvent::Harnesses(list)) => assert!(list.iter().all(|h| h.available)),
            other => panic!("{other:?}"),
        }
        assert_eq!(reg.launcher.programs.get(Harness::Codex), "/new/codex");
        assert_eq!(
            reg.launcher.programs.get(Harness::OpenCode),
            "/new/opencode"
        );
        assert_eq!(
            reg.launcher.programs.get(Harness::Claude),
            "claude",
            "an available CLI is not looked up again"
        );
        assert!(tmp.path().join("opencode/plugins/termist.ts").is_file());
    }

    #[tokio::test]
    async fn a_rescan_that_finds_nothing_changes_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |_| None;
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        let found = reg.rescans_rx.as_mut().unwrap().recv().await.unwrap();
        reg.rescanned(found);
        assert!(rx.try_recv().is_err(), "nothing to tell");
        assert!(!reg.rescanning, "the next rescan may run");
    }

    // Every `n` and `p` asks for a rescan; with a CLI missing, each would start a login
    // shell again. One rescan per window is enough.
    #[tokio::test]
    async fn a_rescan_runs_at_most_once_per_window() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |_| None;
        let mut results = reg.rescans_rx.take().unwrap();
        let t0 = std::time::Instant::now();
        reg.rescan(t0);
        reg.rescanned(results.recv().await.unwrap());
        reg.rescan(t0 + Duration::from_secs(10));
        assert!(!reg.rescanning, "too soon after the last one");
        tokio::task::yield_now().await;
        assert!(results.try_recv().is_err());
        reg.rescan(t0 + RESCAN_INTERVAL);
        assert!(reg.rescanning, "the window has passed");
        results.recv().await.unwrap();
    }

    fn running_claude(reg: &mut Registry, p: &ProjectInfo) -> SessionId {
        let s = claude(p, "c-1");
        reg.sessions.push(Session::new(s.clone(), None, true));
        reg.session_mut(s.id).unwrap().info.status = AgentStatus::Running;
        s.id
    }

    fn later(ms: u64) -> std::time::Instant {
        std::time::Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn an_idle_title_with_no_hook_for_a_while_cancels_a_running_claude_turn() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✶ Fix login".into())));
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.poll_idle_titles(later(1000));
        assert_eq!(
            reg.session(id).unwrap().info.status,
            AgentStatus::Running,
            "too early"
        );
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
    }

    #[test]
    fn a_hook_or_a_working_title_keeps_the_turn_running() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.hook(id, Harness::Claude, "PostToolUse", &Value::Null);
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
        reg.note(SessionNote::Title(id, Some("✳ Fix login again".into())));
        reg.note(SessionNote::Title(id, Some("✻ Fix login again".into())));
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
    }

    #[test]
    fn an_idle_title_while_waiting_for_the_user_cancels_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.session_mut(id).unwrap().info.status = AgentStatus::NeedsFeedback;
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.poll_idle_titles(later(5000));
        assert_eq!(
            reg.session(id).unwrap().info.status,
            AgentStatus::NeedsFeedback
        );
    }
}
