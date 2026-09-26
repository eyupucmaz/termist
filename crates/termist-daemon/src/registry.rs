use crate::claude;
use crate::launch::{LaunchRequest, Launcher};
use crate::session::{self, ClientId, SessionCmd, SessionNote};
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, ProjectId, ProjectInfo, ServerEvent,
    SessionId, SessionInfo, SessionKind, Signal, StateSnapshot, now_ms,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

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
                if event == "SessionStart"
                    && let (Some(s), Some(sid)) = (
                        self.session_mut(session),
                        payload.get("session_id").and_then(Value::as_str),
                    )
                {
                    s.info.agent_session_id = Some(sid.to_string());
                }
                let signal = match harness {
                    Harness::Claude => claude::signal_for(&event, &payload),
                    Harness::Codex | Harness::OpenCode => None,
                };
                if let Some(signal) = signal {
                    self.signal(session, signal);
                }
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
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => reg.handle(msg),
            Some(note) = notes.recv() => reg.note(note),
            else => break,
        }
    }
}
