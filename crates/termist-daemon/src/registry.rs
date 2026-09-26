use crate::launch::{LaunchRequest, Launcher};
use crate::session::{self, ClientId, SessionCmd, SessionNote};
use crate::transcript::TranscriptTail;
use crate::{claude, codex, opencode};
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, ProjectId, ProjectInfo, ServerEvent,
    SessionId, SessionInfo, SessionKind, Signal, StateSnapshot, now_ms,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

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
    cmd: UnboundedSender<SessionCmd>,
    transcript: Option<TranscriptTail>,
}

pub struct Registry {
    launcher: Launcher,
    harnesses: Vec<HarnessInfo>,
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
        notes: UnboundedSender<SessionNote>,
        shutdown: oneshot::Sender<()>,
    ) -> Registry {
        Registry {
            launcher,
            harnesses,
            projects: vec![],
            sessions: vec![],
            clients: HashMap::new(),
            notes,
            shutdown: Some(shutdown),
            created: 0,
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
                    let _ = s.cmd.send(SessionCmd::Detach { client });
                }
            }
            Msg::Request { client, req } => self.request(client, req),
        }
    }

    pub fn note(&mut self, note: SessionNote) {
        match note {
            SessionNote::Title(id, title) => {
                if let Some(s) = self.session_mut(id) {
                    s.info.title = title;
                    let info = s.info.clone();
                    self.broadcast(ServerEvent::SessionUpdated(info));
                }
            }
            SessionNote::Activity(id) => {
                if let Some(s) = self.session_mut(id) {
                    s.info.last_activity_ms = now_ms();
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
                if let (Some(s), Some(out)) = (self.session(session), self.clients.get(&client)) {
                    let _ = s.cmd.send(SessionCmd::Resize { cols, rows });
                    let _ = s.cmd.send(SessionCmd::Attach {
                        client,
                        out: out.clone(),
                    });
                }
            }
            ClientRequest::Detach { session } => {
                if let Some(s) = self.session(session) {
                    let _ = s.cmd.send(SessionCmd::Detach { client });
                }
            }
            ClientRequest::Input { session, data } => {
                if let Some(s) = self.session(session) {
                    let _ = s.cmd.send(SessionCmd::Input(data));
                }
                self.signal(session, Signal::UserTyped);
            }
            ClientRequest::Resize {
                session,
                cols,
                rows,
            } => {
                if let Some(s) = self.session(session) {
                    let _ = s.cmd.send(SessionCmd::Resize { cols, rows });
                }
            }
            ClientRequest::MarkSeen { session } => self.signal(session, Signal::Seen),
            ClientRequest::KillSession { session } => {
                if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session) {
                    let s = self.sessions.remove(pos);
                    let _ = s.cmd.send(SessionCmd::Kill);
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
            ClientRequest::Resume { .. } => self.send(
                client,
                ServerEvent::Error {
                    message: "resume is not supported yet".into(),
                },
            ),
            ClientRequest::Shutdown => {
                for s in &self.sessions {
                    let _ = s.cmd.send(SessionCmd::Kill);
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
                if event == "SessionStart"
                    && let Some(sid) = payload.get("session_id").and_then(Value::as_str)
                {
                    self.set_agent_session_id(id, sid);
                }
                if let Some(path) = payload.get("transcript_path").and_then(Value::as_str) {
                    self.watch_transcript(id, Path::new(path));
                }
                claude::signal_for(event, payload)
            }
            Harness::Codex => {
                if event == "SessionStart"
                    && let Some(sid) = payload.get("session_id").and_then(Value::as_str)
                {
                    self.set_agent_session_id(id, sid);
                }
                codex::signal_for(event, payload)
            }
            Harness::OpenCode => {
                if event == "session.created" {
                    let known = self
                        .session(id)
                        .and_then(|s| s.info.agent_session_id.clone());
                    if known.is_none()
                        && !opencode::is_child_session(payload)
                        && let Some(sid) = opencode::event_session(payload)
                    {
                        self.set_agent_session_id(id, sid);
                    }
                    None
                } else if self.is_foreign_opencode_event(id, payload) {
                    None
                } else {
                    opencode::signal_for(event, payload)
                }
            }
        };
        if let Some(signal) = signal {
            self.signal(id, signal);
        }
    }

    fn set_agent_session_id(&mut self, id: SessionId, sid: &str) {
        if let Some(s) = self.session_mut(id)
            && s.info.agent_session_id.as_deref() != Some(sid)
        {
            s.info.agent_session_id = Some(sid.to_string());
            let info = s.info.clone();
            self.broadcast(ServerEvent::SessionUpdated(info));
        }
    }

    /// An OpenCode event about a session other than this card's (a subagent's).
    fn is_foreign_opencode_event(&self, id: SessionId, payload: &Value) -> bool {
        let known = self
            .session(id)
            .and_then(|s| s.info.agent_session_id.as_deref());
        matches!((known, opencode::event_session(payload)), (Some(k), Some(e)) if k != e)
    }

    fn watch_transcript(&mut self, id: SessionId, path: &Path) {
        if let Some(s) = self.session_mut(id)
            && s.transcript.as_ref().is_none_or(|t| t.path() != path)
        {
            s.transcript = Some(TranscriptTail::new(path.to_path_buf()));
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
        self.sessions.push(Session {
            info: info.clone(),
            cmd,
            transcript: None,
        });
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
