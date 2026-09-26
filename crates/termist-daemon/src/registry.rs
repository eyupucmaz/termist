use crate::launch::{LaunchRequest, Launcher};
use crate::session::{self, ClientId, SessionCmd, SessionNote};
use crate::store::{Store, StoredSession};
use crate::transcript::TranscriptTail;
use crate::{claude, codex, opencode};
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, ProjectId, ProjectInfo, ServerEvent,
    SessionId, SessionInfo, SessionKind, Signal, StateSnapshot, now_ms,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

/// How often a session's PTY activity is broadcast to clients as `SessionUpdated`.
const ACTIVITY_BROADCAST: Duration = Duration::from_secs(5);

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
        }
    }
}

pub struct Registry {
    launcher: Launcher,
    harnesses: Vec<HarnessInfo>,
    store: Store,
    projects: Vec<ProjectInfo>,
    sessions: Vec<Session>,
    clients: HashMap<ClientId, UnboundedSender<ServerEvent>>,
    notes: UnboundedSender<SessionNote>,
    shutdown: Option<oneshot::Sender<()>>,
    created: u32,
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
        Registry {
            launcher,
            harnesses,
            store,
            projects,
            sessions: sessions
                .into_iter()
                .map(|StoredSession { info, resumable }| Session::new(info, None, resumable))
                .collect(),
            clients: HashMap::new(),
            notes,
            shutdown: Some(shutdown),
            created,
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
                cols,
                rows,
            } => {
                if let Err(e) = self.create_session(project, kind, prompt, cols, rows) {
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
        let signal = match harness {
            Harness::Claude => {
                // Claude's id is known before its conversation exists: not a sign of one.
                self.capture_session_start(id, event, payload);
                if let Some(path) = payload.get("transcript_path").and_then(Value::as_str) {
                    self.watch_transcript(id, Path::new(path));
                }
                if event == "UserPromptSubmit" {
                    self.skip_transcript_so_far(id);
                }
                claude::signal_for(event, payload)
            }
            Harness::Codex => {
                if self.capture_session_start(id, event, payload) {
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

    fn add_project(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let path = std::fs::canonicalize(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        if self.projects.iter().any(|p| p.path == path) {
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
        });
        if let Err(e) = self.store.upsert_project(self.projects.last().unwrap()) {
            tracing::warn!(error = %e, "could not store project");
        }
        Ok(())
    }

    fn create_session(
        &mut self,
        project: ProjectId,
        kind: SessionKind,
        prompt: Option<String>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let Some(proj) = self.projects.iter().find(|p| p.id == project) else {
            bail!("unknown project")
        };
        let id = SessionId::new();
        let launch = self.launcher.launch(LaunchRequest {
            id,
            kind: &kind,
            prompt: prompt.as_deref(),
            cwd: &proj.path,
            cols,
            rows,
            resume: None,
        });
        let program = launch.spec.program.clone();
        let cmd = session::spawn(launch.spec, self.notes.clone())
            .with_context(|| format!("could not start {} ({program})", kind.label()))?;
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
        };
        self.sessions
            .push(Session::new(info.clone(), Some(cmd), false));
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
        Ok(())
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
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(msg) => reg.handle(msg),
                None => break,
            },
            Some(note) = notes.recv() => reg.note(note),
            _ = transcripts.tick() => reg.poll_transcripts(),
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
}
