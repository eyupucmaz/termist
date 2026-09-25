use crate::encode::{encode_key, encode_paste};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use termist_core::{
    AgentStatus, ClientRequest, Harness, ProjectId, ServerEvent, SessionId, SessionInfo,
    SessionKind, Snapshot, StateSnapshot, next_in_attention,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Grid,
    Focus,
    FocusPrefix,
    ConfirmQuit,
    /// `d` was pressed on this session; `y` / Enter kills it, any other key cancels.
    ConfirmKill(SessionId),
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Send(ClientRequest),
    Quit,
}

pub struct App {
    pub state: StateSnapshot,
    pub project: Option<ProjectId>,
    pub selected: Option<SessionId>,
    pub mode: Mode,
    pub screens: HashMap<SessionId, Snapshot>,
    pub attached: Option<SessionId>,
    pub pane: (u16, u16),
    pub cards_per_row: usize,
    pub message: Option<String>,
    /// Set by the first `State` from the daemon; until then the body says "Connecting…".
    pub connected: bool,
    focus_next_created: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> App {
        App {
            state: StateSnapshot::default(),
            project: None,
            selected: None,
            mode: Mode::Grid,
            screens: HashMap::new(),
            attached: None,
            pane: (0, 0),
            cards_per_row: 1,
            message: None,
            connected: false,
            focus_next_created: false,
        }
    }

    pub fn project_sessions(&self) -> Vec<&SessionInfo> {
        self.state
            .sessions
            .iter()
            .filter(|s| Some(s.project) == self.project)
            .collect()
    }

    pub fn selected_info(&self) -> Option<&SessionInfo> {
        self.selected
            .and_then(|id| self.state.sessions.iter().find(|s| s.id == id))
    }

    pub fn on_event(&mut self, event: ServerEvent) -> Vec<Action> {
        let mut actions = Vec::new();
        match event {
            ServerEvent::State(state) => {
                self.state = state;
                self.connected = true;
                self.repair_selection();
            }
            ServerEvent::SessionUpdated(info) => {
                let id = info.id;
                // You are looking at it (PRD §8): a focused session that finishes is seen.
                let seen_now = info.status == AgentStatus::Unseen
                    && self.selected == Some(id)
                    && self.attached == Some(id)
                    && matches!(self.mode, Mode::Focus | Mode::FocusPrefix);
                let known = self.state.sessions.iter().any(|s| s.id == id);
                match self.state.sessions.iter_mut().find(|s| s.id == id) {
                    Some(s) => *s = info,
                    None => self.state.sessions.push(info),
                }
                if !known && self.focus_next_created {
                    self.focus_next_created = false;
                    self.select(id);
                    self.mode = Mode::Focus;
                }
                self.repair_selection();
                if seen_now {
                    actions.push(Action::Send(ClientRequest::MarkSeen { session: id }));
                }
            }
            ServerEvent::SessionRemoved(id) => {
                self.state.sessions.retain(|s| s.id != id);
                self.screens.remove(&id);
                if self.attached == Some(id) {
                    self.attached = None;
                }
                if self.selected == Some(id) {
                    self.selected = None;
                    if matches!(self.mode, Mode::Focus | Mode::FocusPrefix) {
                        self.mode = Mode::Grid;
                    }
                }
                if self.mode == Mode::ConfirmKill(id) {
                    self.mode = Mode::Grid;
                }
                self.repair_selection();
            }
            ServerEvent::Screen { session, update } => {
                self.screens.entry(session).or_default().apply(&update);
            }
            ServerEvent::Error { message } => {
                self.focus_next_created = false;
                self.message = Some(message);
            }
            ServerEvent::Hello { .. } | ServerEvent::Ack => {}
        }
        actions.extend(self.sync_attachment());
        actions
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut actions = Vec::new();
        match self.mode {
            Mode::ConfirmQuit => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
                    || (ctrl && key.code == KeyCode::Char('c'))
                {
                    return vec![Action::Quit];
                }
                self.mode = Mode::Grid;
                return vec![];
            }
            Mode::ConfirmKill(id) => {
                self.mode = Mode::Grid;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                    return vec![Action::Send(ClientRequest::KillSession { session: id })];
                }
                return vec![];
            }
            Mode::Focus => {
                if ctrl && key.code == KeyCode::Char('q') {
                    self.mode = Mode::Grid;
                } else if ctrl && key.code == KeyCode::Char('a') {
                    self.mode = Mode::FocusPrefix;
                } else if let Some(id) = self.selected {
                    let modes = self.screens.get(&id).map(|s| s.modes).unwrap_or_default();
                    let data = encode_key(&key, &modes);
                    if !data.is_empty() {
                        actions.push(Action::Send(ClientRequest::Input { session: id, data }));
                    }
                }
            }
            Mode::FocusPrefix => {
                self.mode = Mode::Focus;
                match key.code {
                    // Esc, q, and C-q (PRD §7: C-q always escapes, prefix or not).
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Grid,
                    KeyCode::Char('a') if ctrl => {
                        if let Some(id) = self.selected {
                            actions.push(Action::Send(ClientRequest::Input {
                                session: id,
                                data: vec![0x01],
                            }));
                        }
                    }
                    KeyCode::Char(c @ ('.' | ',' | 'h' | 'j' | 'k' | 'l')) => self.navigate(c),
                    _ => {}
                }
            }
            Mode::Grid => {
                // An error message stays up only until the next key.
                self.message = None;
                match key.code {
                    KeyCode::Char('c') if ctrl => self.mode = Mode::ConfirmQuit,
                    KeyCode::Char('q') => self.mode = Mode::ConfirmQuit,
                    KeyCode::Enter if self.selected.is_some() => self.mode = Mode::Focus,
                    KeyCode::Char('n') => actions.extend(self.create(SessionKind::Agent {
                        harness: Harness::Claude,
                    })),
                    KeyCode::Char('t') => actions.extend(self.create(SessionKind::Shell)),
                    KeyCode::Char('d') => {
                        if let Some(id) = self.selected {
                            self.mode = Mode::ConfirmKill(id);
                        }
                    }
                    KeyCode::Char(']') => self.switch_project(1),
                    KeyCode::Char('[') => self.switch_project(-1),
                    KeyCode::Char(c @ '1'..='9') => {
                        if let Some(p) = self.state.projects.get(c as usize - '1' as usize) {
                            self.project = Some(p.id);
                            self.selected = None;
                            self.repair_selection();
                        }
                    }
                    KeyCode::Char(c @ ('.' | ',' | 'h' | 'j' | 'k' | 'l')) => self.navigate(c),
                    _ => {}
                }
            }
        }
        actions.extend(self.sync_attachment());
        actions
    }

    pub fn on_paste(&mut self, text: &str) -> Vec<Action> {
        match (self.mode, self.selected) {
            (Mode::Focus, Some(id)) => {
                let modes = self.screens.get(&id).map(|s| s.modes).unwrap_or_default();
                vec![Action::Send(ClientRequest::Input {
                    session: id,
                    data: encode_paste(text, &modes),
                })]
            }
            _ => vec![],
        }
    }

    pub fn pane_resized(&mut self, cols: u16, rows: u16) -> Vec<Action> {
        if (cols, rows) == self.pane {
            return vec![];
        }
        self.pane = (cols, rows);
        if cols == 0 || rows == 0 {
            return vec![];
        }
        let mut actions = Vec::new();
        if let Some(id) = self.attached {
            actions.push(Action::Send(ClientRequest::Resize {
                session: id,
                cols,
                rows,
            }));
        }
        actions.extend(self.sync_attachment());
        actions
    }

    fn create(&mut self, kind: SessionKind) -> Vec<Action> {
        let Some(project) = self.project else {
            self.message = Some("no project open".into());
            return vec![];
        };
        self.focus_next_created = true;
        let (cols, rows) = self.pane;
        vec![Action::Send(ClientRequest::CreateSession {
            project,
            kind,
            prompt: None,
            cols: cols.max(20),
            rows: rows.max(5),
        })]
    }

    fn navigate(&mut self, c: char) {
        match c {
            '.' | ',' => {
                if let Some(id) = next_in_attention(&self.state.sessions, self.selected, c == '.') {
                    self.select(id);
                }
            }
            'h' => self.move_by(-1),
            'l' => self.move_by(1),
            'j' => self.move_by(self.cards_per_row.max(1) as isize),
            'k' => self.move_by(-(self.cards_per_row.max(1) as isize)),
            _ => {}
        }
    }

    fn move_by(&mut self, delta: isize) {
        let ids: Vec<SessionId> = self.project_sessions().iter().map(|s| s.id).collect();
        if ids.is_empty() {
            return;
        }
        let pos = self
            .selected
            .and_then(|id| ids.iter().position(|x| *x == id))
            .unwrap_or(0) as isize;
        let next = (pos + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.selected = Some(ids[next]);
    }

    fn switch_project(&mut self, delta: isize) {
        let n = self.state.projects.len() as isize;
        if n == 0 {
            return;
        }
        let pos = self
            .project
            .and_then(|p| self.state.projects.iter().position(|x| x.id == p))
            .unwrap_or(0) as isize;
        self.project = Some(self.state.projects[((pos + delta).rem_euclid(n)) as usize].id);
        self.selected = None;
        self.repair_selection();
    }

    fn select(&mut self, id: SessionId) {
        if let Some(s) = self.state.sessions.iter().find(|s| s.id == id) {
            self.project = Some(s.project);
            self.selected = Some(id);
        }
    }

    /// Keeps `project` and `selected` pointing at things that exist.
    fn repair_selection(&mut self) {
        if !self
            .project
            .is_some_and(|p| self.state.projects.iter().any(|x| x.id == p))
        {
            self.project = self.state.projects.first().map(|p| p.id);
        }
        let valid = self
            .selected
            .is_some_and(|id| self.project_sessions().iter().any(|s| s.id == id));
        if !valid {
            self.selected = self.project_sessions().first().map(|s| s.id);
        }
    }

    fn sync_attachment(&mut self) -> Vec<Action> {
        let (cols, rows) = self.pane;
        if self.selected == self.attached || cols == 0 || rows == 0 {
            return vec![];
        }
        let mut actions = Vec::new();
        if let Some(old) = self.attached.take() {
            actions.push(Action::Send(ClientRequest::Detach { session: old }));
        }
        if let Some(new) = self.selected {
            actions.push(Action::Send(ClientRequest::Attach {
                session: new,
                cols,
                rows,
            }));
            self.attached = Some(new);
            if self
                .selected_info()
                .is_some_and(|s| s.status == AgentStatus::Unseen)
            {
                actions.push(Action::Send(ClientRequest::MarkSeen { session: new }));
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};
    use termist_core::{ProjectId, ProjectInfo};

    fn k(code: K) -> KeyEvent {
        KeyEvent::new(code, M::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(K::Char(c), M::CONTROL)
    }

    fn session(project: ProjectId, name: &str, status: AgentStatus) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project,
            kind: SessionKind::Shell,
            name: name.into(),
            status,
            agent_session_id: None,
            title: None,
            last_activity_ms: 1,
        }
    }

    /// Two projects: "api" with three sessions, "web" with one waiting on the user.
    fn app() -> (App, Vec<SessionInfo>) {
        let api = ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: "/api".into(),
        };
        let web = ProjectInfo {
            id: ProjectId::new(),
            name: "web".into(),
            path: "/web".into(),
        };
        let s = vec![
            session(api.id, "a1", AgentStatus::Finished),
            session(api.id, "a2", AgentStatus::Unseen),
            session(api.id, "a3", AgentStatus::Running),
            session(web.id, "w1", AgentStatus::NeedsFeedback),
        ];
        let mut app = App::new();
        app.pane_resized(80, 20);
        app.cards_per_row = 2;
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![api, web],
            sessions: s.clone(),
        }));
        (app, s)
    }

    fn sent(actions: &[Action]) -> Vec<&ClientRequest> {
        actions
            .iter()
            .filter_map(|a| {
                if let Action::Send(r) = a {
                    Some(r)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn state_selects_the_first_project_and_session_and_attaches() {
        let mut app = App::new();
        app.pane_resized(80, 20);
        let (_, s) = self::app();
        let p = ProjectInfo {
            id: s[0].project,
            name: "api".into(),
            path: "/api".into(),
        };
        let actions = app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s[..3].to_vec(),
        }));
        assert_eq!(app.selected, Some(s[0].id));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::Attach {
                session: s[0].id,
                cols: 80,
                rows: 20
            }]
        );
    }

    #[test]
    fn hjkl_moves_within_the_project_and_clamps() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('l')));
        assert_eq!(app.selected, Some(s[1].id));
        app.on_key(k(K::Char('j')));
        assert_eq!(
            app.selected,
            Some(s[2].id),
            "j moves one row of two cards, clamped at the end"
        );
        app.on_key(k(K::Char('l')));
        assert_eq!(app.selected, Some(s[2].id));
        app.on_key(k(K::Char('k')));
        assert_eq!(app.selected, Some(s[0].id));
    }

    #[test]
    fn selecting_an_unread_card_marks_it_seen_and_switches_attachment() {
        let (mut app, s) = app();
        let actions = app.on_key(k(K::Char('l')));
        assert_eq!(
            sent(&actions),
            vec![
                &ClientRequest::Detach { session: s[0].id },
                &ClientRequest::Attach {
                    session: s[1].id,
                    cols: 80,
                    rows: 20
                },
                &ClientRequest::MarkSeen { session: s[1].id },
            ]
        );
    }

    #[test]
    fn dot_jumps_to_the_waiting_session_in_another_project() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('.')));
        assert_eq!(app.selected, Some(s[3].id));
        assert_eq!(app.project, Some(s[3].project));
    }

    #[test]
    fn focus_mode_sends_keys_and_the_prefix_gets_you_out() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        let actions = app.on_key(k(K::Char('x')));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::Input {
                session: s[0].id,
                data: b"x".to_vec()
            }]
        );
        app.on_key(ctrl('a'));
        assert_eq!(app.mode, Mode::FocusPrefix);
        let actions = app.on_key(ctrl('a'));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::Input {
                session: s[0].id,
                data: vec![0x01]
            }]
        );
        assert_eq!(app.mode, Mode::Focus);
        app.on_key(ctrl('a'));
        app.on_key(k(K::Esc));
        assert_eq!(app.mode, Mode::Grid);
        app.on_key(k(K::Enter));
        app.on_key(ctrl('q'));
        assert_eq!(app.mode, Mode::Grid, "Ctrl+q always escapes");
    }

    #[test]
    fn prefix_dot_moves_to_the_next_waiting_session_and_stays_focused() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('.')));
        assert_eq!(app.selected, Some(s[3].id));
        assert_eq!(app.mode, Mode::Focus);
    }

    #[test]
    fn new_session_is_requested_at_pane_size_and_focused_when_it_arrives() {
        let (mut app, s) = app();
        let actions = app.on_key(k(K::Char('n')));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::CreateSession {
                project: s[0].project,
                kind: SessionKind::Agent {
                    harness: Harness::Claude
                },
                prompt: None,
                cols: 80,
                rows: 20
            }]
        );
        let fresh = session(s[0].project, "claude-5", AgentStatus::Fresh);
        app.on_event(ServerEvent::SessionUpdated(fresh.clone()));
        assert_eq!(app.selected, Some(fresh.id));
        assert_eq!(app.mode, Mode::Focus);
    }

    #[test]
    fn quitting_asks_first() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('q')));
        assert_eq!(app.mode, Mode::ConfirmQuit);
        assert!(app.on_key(k(K::Char('n'))).is_empty());
        assert_eq!(app.mode, Mode::Grid);
        app.on_key(ctrl('c'));
        assert_eq!(app.on_key(ctrl('c')), vec![Action::Quit]);
    }

    #[test]
    fn killing_a_session_asks_first_and_any_other_key_cancels() {
        let (mut app, _) = app();
        assert!(sent(&app.on_key(k(K::Char('d')))).is_empty());
        assert!(matches!(app.mode, Mode::ConfirmKill(_)));
        assert!(sent(&app.on_key(k(K::Char('n')))).is_empty());
        assert_eq!(app.mode, Mode::Grid);
        let (mut app, s) = self::app();
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::SessionRemoved(s[0].id));
        assert_eq!(app.mode, Mode::Grid, "the session to kill went away");
    }

    #[test]
    fn killing_a_session_is_confirmed_with_y_or_enter() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('d')));
        assert_eq!(app.mode, Mode::ConfirmKill(s[0].id));
        assert_eq!(
            sent(&app.on_key(k(K::Char('y')))),
            vec![&ClientRequest::KillSession { session: s[0].id }]
        );
        assert_eq!(app.mode, Mode::Grid);
        app.on_key(k(K::Char('d')));
        assert_eq!(
            sent(&app.on_key(k(K::Enter))),
            vec![&ClientRequest::KillSession { session: s[0].id }]
        );
    }

    #[test]
    fn a_focused_session_that_finishes_is_marked_seen_at_once() {
        let (mut app, s) = app();
        let mut running = s[2].clone();
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Enter));
        assert_eq!((app.mode, app.attached), (Mode::Focus, Some(running.id)));
        running.status = AgentStatus::Unseen;
        let actions = app.on_event(ServerEvent::SessionUpdated(running.clone()));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::MarkSeen {
                session: running.id
            }]
        );
        app.on_key(ctrl('a'));
        assert_eq!(app.mode, Mode::FocusPrefix);
        let actions = app.on_event(ServerEvent::SessionUpdated(running.clone()));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::MarkSeen {
                session: running.id
            }],
            "also while the prefix is pending"
        );
    }

    #[test]
    fn a_selected_session_that_finishes_in_the_grid_stays_unseen() {
        let (mut app, s) = app();
        let mut first = s[0].clone();
        first.status = AgentStatus::Unseen;
        assert!(sent(&app.on_event(ServerEvent::SessionUpdated(first))).is_empty());
    }

    #[test]
    fn ctrl_q_after_the_prefix_still_escapes_to_the_grid() {
        let (mut app, _) = app();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        assert_eq!(app.mode, Mode::FocusPrefix);
        assert!(sent(&app.on_key(ctrl('q'))).is_empty());
        assert_eq!(app.mode, Mode::Grid, "C-q always escapes (PRD §7)");
    }

    #[test]
    fn an_error_message_clears_on_the_next_grid_key() {
        let (mut app, _) = app();
        app.on_event(ServerEvent::Error {
            message: "boom".into(),
        });
        assert_eq!(app.message.as_deref(), Some("boom"));
        app.on_key(k(K::Char('l')));
        assert_eq!(app.message, None);
    }

    #[test]
    fn removing_the_focused_session_returns_to_the_grid() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        app.on_event(ServerEvent::SessionRemoved(s[0].id));
        assert_eq!(app.mode, Mode::Grid);
        assert_eq!(app.selected, Some(s[1].id));
    }

    // Review Focus 5
    #[test]
    fn a_zero_sized_pane_never_resizes_or_attaches() {
        let (mut app, _) = app();
        assert!(app.pane_resized(80, 0).is_empty());
        let mut fresh = App::new();
        fresh.pane_resized(0, 0);
        let (_, s) = self::app();
        let p = ProjectInfo {
            id: s[0].project,
            name: "api".into(),
            path: "/api".into(),
        };
        let actions = fresh.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s[..1].to_vec(),
        }));
        assert!(sent(&actions).is_empty());
    }

    #[test]
    fn screen_updates_are_kept_per_session() {
        let (mut app, s) = app();
        let mut snap = Snapshot::blank(3, 1);
        snap.lines[0][0].ch = 'h';
        let update = termist_core::screen::diff(None, &snap).unwrap();
        app.on_event(ServerEvent::Screen {
            session: s[0].id,
            update,
        });
        assert_eq!(app.screens[&s[0].id].line_text(0), "h");
    }
}
