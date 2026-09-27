use crate::browse::Listing;
use crate::encode::{encode_key, encode_paste};
use crate::list_picker::{ListPicker, Pick};
use crate::overlay::{
    self, BrowseEntry, ModelChoice, ModelPicker, OpenProject, Overlay, QuickPrompt,
};
use crate::text_input::{Edit, TextInput};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, LaunchOptions, ProjectId, ProjectInfo,
    ServerEvent, SessionId, SessionInfo, SessionKind, Snapshot, StateSnapshot, attention_order,
    next_in_attention,
};

/// How many earlier prompts the quick prompt asks for.
const PROMPT_HISTORY: u32 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Grid,
    Focus,
    FocusPrefix,
    ConfirmQuit,
    /// `d` was pressed on this session; `y` / Enter kills it, any other key cancels.
    ConfirmKill(SessionId),
    /// `x` was pressed on this project tab; `y` / Enter closes it.
    ConfirmClose(ProjectId),
    /// `a` was pressed on this session; `y` / Enter archives it.
    ConfirmArchive(SessionId),
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Send(ClientRequest),
    /// List a folder off the UI thread; the result comes back through `App::listed`.
    ListDir(PathBuf),
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
    /// Rows of cards that fit on screen, and the first one shown.
    pub card_rows: usize,
    pub card_scroll: usize,
    /// `A`: the grid shows the project's archived cards instead of its live ones.
    pub archive_view: bool,
    pub message: Option<String>,
    /// Set by the first `State` from the daemon; until then the body says "Connecting…".
    pub connected: bool,
    /// The agent CLIs the daemon can launch; the default is all three, until the
    /// daemon's `Harnesses` event arrives.
    pub harnesses: Vec<HarnessInfo>,
    /// Pickers and text boxes on top of the grid or pane; the last one gets the keys.
    pub overlays: Vec<Overlay>,
    /// Earlier prompts, newest first, as the daemon last sent them.
    prompt_history: Vec<String>,
    /// Recently used models per harness, as the daemon last sent them.
    recent_models: HashMap<Harness, Vec<String>>,
    focus_next_created: bool,
    resume_pending: Option<SessionId>,
    /// A project asked to be opened (or a folder added); switched to when it arrives.
    project_pending: Option<ProjectPending>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProjectPending {
    Known(ProjectId),
    /// A folder added as shown, and the projects that were open when it was asked for.
    Path {
        path: PathBuf,
        open_before: Vec<ProjectId>,
    },
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
            card_rows: 1,
            card_scroll: 0,
            archive_view: false,
            message: None,
            connected: false,
            harnesses: Harness::ALL
                .into_iter()
                .map(|harness| HarnessInfo {
                    harness,
                    available: true,
                })
                .collect(),
            overlays: Vec::new(),
            prompt_history: Vec::new(),
            recent_models: HashMap::new(),
            focus_next_created: false,
            resume_pending: None,
            project_pending: None,
        }
    }

    /// The cards of the current project: its archived ones in the archive view, the
    /// others everywhere else.
    pub fn project_sessions(&self) -> Vec<&SessionInfo> {
        self.state
            .sessions
            .iter()
            .filter(|s| Some(s.project) == self.project && s.archived == self.archive_view)
            .collect()
    }

    /// The projects shown as tabs.
    pub fn open_projects(&self) -> impl Iterator<Item = &ProjectInfo> {
        self.state.projects.iter().filter(|p| p.open)
    }

    /// Sessions the palette and `.` / `,` can reach: not archived, in an open project.
    pub fn visible_sessions(&self) -> Vec<SessionInfo> {
        self.state
            .sessions
            .iter()
            .filter(|s| {
                !s.archived
                    && self
                        .state
                        .projects
                        .iter()
                        .any(|p| p.id == s.project && p.open)
            })
            .cloned()
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
                self.switch_to_pending_project();
                self.repair_selection();
            }
            ServerEvent::SessionUpdated(info) => {
                let (id, status) = (info.id, info.status);
                // Archived elsewhere while focused here: back to the grid.
                if info.archived
                    && !self.archive_view
                    && self.selected == Some(id)
                    && matches!(
                        self.mode,
                        Mode::Focus | Mode::FocusPrefix | Mode::ConfirmArchive(_)
                    )
                {
                    self.mode = Mode::Grid;
                }
                // You are looking at it: a focused session that finishes is seen.
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
                    self.arrived(id);
                }
                self.repair_selection();
                if seen_now {
                    actions.push(Action::Send(ClientRequest::MarkSeen { session: id }));
                }
                if self.resume_pending == Some(id) && status == AgentStatus::Fresh {
                    self.resume_pending = None;
                    self.attached = None; // the old process's attachment ended with it
                    self.arrived(id);
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
                if matches!(self.mode, Mode::ConfirmKill(x) | Mode::ConfirmArchive(x) if x == id) {
                    self.mode = Mode::Grid;
                }
                self.repair_selection();
            }
            ServerEvent::Screen { session, update } => {
                self.screens.entry(session).or_default().apply(&update);
            }
            ServerEvent::Error { message } => {
                self.focus_next_created = false;
                self.resume_pending = None;
                self.project_pending = None;
                self.message = Some(message);
            }
            ServerEvent::Harnesses(list) => {
                for picker in self
                    .overlays
                    .iter_mut()
                    .filter_map(Overlay::harness_picker_mut)
                {
                    picker.set_items(list.clone(), overlay::harness_label);
                }
                self.harnesses = list;
            }
            ServerEvent::PromptHistory(history) => {
                for o in &mut self.overlays {
                    if let Overlay::QuickPrompt(q) = o
                        && q.input.is_empty()
                    {
                        q.input.set_history(history.clone());
                    }
                }
                self.prompt_history = history;
            }
            ServerEvent::Models { harness, recent } => {
                for o in &mut self.overlays {
                    if let Overlay::Model(m) = o
                        && m.harness == harness
                    {
                        m.set_recent(recent.clone(), None);
                    }
                }
                self.recent_models.insert(harness, recent);
            }
            ServerEvent::Hello { .. } | ServerEvent::Ack => {}
        }
        actions.extend(self.sync_attachment());
        actions
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('q') {
            // Ctrl+Q always gets you out: every overlay closes and focus mode ends.
            self.overlays.clear();
            self.mode = Mode::Grid;
            return vec![];
        }
        let mut actions = Vec::new();
        if !self.overlays.is_empty() {
            actions.extend(self.overlay_key(key));
            actions.extend(self.sync_attachment());
            return actions;
        }
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
            Mode::ConfirmClose(project) => {
                self.mode = Mode::Grid;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                    return vec![Action::Send(ClientRequest::CloseProject { project })];
                }
                return vec![];
            }
            Mode::ConfirmArchive(session) => {
                self.mode = Mode::Grid;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                    return vec![Action::Send(ClientRequest::ArchiveSession { session })];
                }
                return vec![];
            }
            Mode::Grid if self.archive_view => {
                self.message = None;
                actions.extend(self.archive_key(key));
            }
            Mode::Focus => {
                if ctrl && key.code == KeyCode::Char('a') {
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
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Grid,
                    KeyCode::Char('a') if ctrl => {
                        if let Some(id) = self.selected {
                            actions.push(Action::Send(ClientRequest::Input {
                                session: id,
                                data: vec![0x01],
                            }));
                        }
                    }
                    KeyCode::Char('p') => actions.extend(self.open_quick_prompt()),
                    KeyCode::Char('/') => self.open_palette(),
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
                    KeyCode::Enter if self.selected.is_some() => actions.extend(self.enter()),
                    KeyCode::Char('n') => actions.extend(self.open_picker()),
                    KeyCode::Char('p') if !ctrl => actions.extend(self.open_quick_prompt()),
                    KeyCode::Char('t') => actions.extend(self.create(SessionKind::Shell)),
                    KeyCode::Char(' ') if !ctrl => self.open_follow_up(),
                    KeyCode::Char('r') if !ctrl => self.open_rename(),
                    KeyCode::Char('a') if !ctrl => {
                        if let Some(id) = self.selected {
                            self.mode = Mode::ConfirmArchive(id);
                        }
                    }
                    KeyCode::Char('A') if !ctrl => self.set_archive_view(true),
                    KeyCode::Char('/') if !ctrl => self.open_palette(),
                    KeyCode::Char('d') if ctrl => self.half_page(1),
                    KeyCode::Char('u') if ctrl => self.half_page(-1),
                    KeyCode::Char('d') => {
                        if let Some(id) = self.selected {
                            self.mode = Mode::ConfirmKill(id);
                        }
                    }
                    KeyCode::Char(']') => self.switch_project(1),
                    KeyCode::Char('[') => self.switch_project(-1),
                    KeyCode::Char(c @ '1'..='9') => {
                        let tab = self
                            .open_projects()
                            .nth(c as usize - '1' as usize)
                            .map(|p| p.id);
                        if let Some(id) = tab {
                            self.project = Some(id);
                            self.selected = None;
                            self.repair_selection();
                        }
                    }
                    KeyCode::Char('o') if !ctrl => actions.extend(self.open_project_browser()),
                    KeyCode::Char('x') if !ctrl => {
                        if let Some(project) = self.project {
                            self.mode = Mode::ConfirmClose(project);
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
        if let Some(top) = self.overlays.last_mut() {
            if let Some(input) = top.text_input_mut() {
                input.insert_str(text);
            }
            return vec![];
        }
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
            model: None,
            effort: None,
            cols: cols.max(20),
            rows: rows.max(5),
        })]
    }

    /// Keys for the overlay on top of the stack.
    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Action> {
        // A message stays up only until the next key.
        self.message = None;
        match self.overlays.last() {
            Some(Overlay::Harness(_)) => self.harness_key(key),
            Some(Overlay::QuickPrompt(_)) => self.quick_prompt_key(key),
            Some(Overlay::Model(_)) => self.model_key(key),
            Some(Overlay::ModelName(_)) => self.model_name_key(key),
            Some(Overlay::Project(_)) => self.project_key(key),
            Some(Overlay::FollowUp { .. }) => self.follow_up_key(key),
            Some(Overlay::Rename { .. }) => self.rename_key(key),
            Some(Overlay::Palette(_)) => self.palette_key(key),
            Some(Overlay::OpenProject(_)) => self.open_project_key(key),
            None => vec![],
        }
    }

    fn open_picker(&mut self) -> Vec<Action> {
        if self.project.is_none() {
            self.message = Some("no project open".into());
            return vec![];
        }
        self.overlays
            .push(Overlay::Harness(overlay::harness_picker(&self.harnesses)));
        // A CLI installed since the daemon started shows up when the answer comes.
        vec![Action::Send(ClientRequest::RescanHarnesses)]
    }

    /// In the quick prompt a harness is chosen for the launch line; otherwise it starts
    /// a session at once.
    fn harness_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Harness(picker)) = self.overlays.last_mut() else {
            return vec![];
        };
        let chosen = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
                return vec![];
            }
            KeyCode::Char(c @ '1'..='9') => Some(c as usize - '1' as usize),
            _ => match picker.key(key) {
                Pick::Chosen => picker.selected_index(),
                _ => None,
            },
        };
        let Some(index) = chosen else {
            return vec![];
        };
        self.overlays.pop();
        let Some(choice) = self.harnesses.get(index).cloned() else {
            return vec![];
        };
        if !choice.available {
            self.message = Some(format!(
                "{} is not installed (not found on PATH)",
                choice.harness.id()
            ));
            return vec![];
        }
        if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
            q.set_harness(choice.harness);
            return vec![];
        }
        self.create(SessionKind::Agent {
            harness: choice.harness,
        })
    }

    fn is_available(&self, harness: Harness) -> bool {
        self.harnesses
            .iter()
            .any(|h| h.harness == harness && h.available)
    }

    /// `p` and `C-a p`: opens on the last launch line (or the first installed CLI), and
    /// asks for the prompt history and for CLIs installed since the daemon started.
    fn open_quick_prompt(&mut self) -> Vec<Action> {
        let Some(project) = self.project else {
            self.message = Some("no project open".into());
            return vec![];
        };
        let launch = self
            .state
            .last_launch
            .clone()
            .filter(|l| self.is_available(l.harness))
            .unwrap_or_else(|| LaunchOptions {
                harness: self
                    .harnesses
                    .iter()
                    .find(|h| h.available)
                    .map_or(Harness::Claude, |h| h.harness),
                model: None,
                effort: None,
            });
        let mut input = TextInput::new(true);
        input.set_history(self.prompt_history.clone());
        self.overlays.push(Overlay::QuickPrompt(QuickPrompt {
            input,
            project,
            launch,
        }));
        vec![
            Action::Send(ClientRequest::ListPromptHistory {
                limit: PROMPT_HISTORY,
            }),
            Action::Send(ClientRequest::RescanHarnesses),
        ]
    }

    fn quick_prompt_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => {
                self.overlays.pop();
            }
            KeyCode::Tab => {
                let current = q.launch.harness;
                let mut picker = overlay::harness_picker(&self.harnesses);
                if let Some(i) = self.harnesses.iter().position(|h| h.harness == current) {
                    picker.select_index(i);
                }
                self.overlays.push(Overlay::Harness(picker));
            }
            KeyCode::Char('o') if ctrl => {
                let launch = q.launch.clone();
                let recent = self
                    .recent_models
                    .get(&launch.harness)
                    .cloned()
                    .unwrap_or_default();
                self.overlays
                    .push(Overlay::Model(ModelPicker::new(&launch, recent)));
                return vec![Action::Send(ClientRequest::ListModels {
                    harness: launch.harness,
                })];
            }
            KeyCode::Char('p') if ctrl => {
                let current = q.project;
                let open = self.open_projects().cloned().collect();
                self.overlays
                    .push(Overlay::Project(overlay::project_picker(open, current)));
            }
            _ => {
                if q.input.key(key) == Edit::Submit {
                    return self.submit_quick_prompt();
                }
            }
        }
        vec![]
    }

    /// Starts the task. An empty prompt starts the CLI bare; a CLI that is not
    /// installed starts nothing and the prompt stays open.
    fn submit_quick_prompt(&mut self) -> Vec<Action> {
        let Some(Overlay::QuickPrompt(q)) = self.overlays.last() else {
            return vec![];
        };
        let harness = q.launch.harness;
        if !self.is_available(harness) {
            self.message = Some(format!(
                "{} is not installed (not found on PATH)",
                harness.id()
            ));
            return vec![];
        }
        let Some(Overlay::QuickPrompt(q)) = self.overlays.pop() else {
            return vec![];
        };
        let text = q.input.text();
        let prompt = (!text.trim().is_empty()).then(|| text.to_string());
        self.focus_next_created = true;
        let (cols, rows) = self.pane;
        vec![
            Action::Send(ClientRequest::SetLastLaunch(q.launch.clone())),
            Action::Send(ClientRequest::CreateSession {
                project: q.project,
                kind: SessionKind::Agent { harness },
                prompt,
                model: q.launch.model,
                effort: q.launch.effort,
                cols: cols.max(20),
                rows: rows.max(5),
            }),
        ]
    }

    fn model_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Model(m)) = self.overlays.last_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
            }
            KeyCode::Left | KeyCode::Char('h') => m.step_effort(-1),
            KeyCode::Right | KeyCode::Char('l') => m.step_effort(1),
            _ => {
                if m.models.key(key) == Pick::Chosen {
                    match m.models.selected().cloned() {
                        Some(ModelChoice::Type) => self
                            .overlays
                            .push(Overlay::ModelName(TextInput::new(false))),
                        Some(choice) => {
                            let effort = m.effort();
                            self.overlays.pop();
                            self.set_model(choice.model(), effort);
                        }
                        None => {}
                    }
                }
            }
        }
        vec![]
    }

    /// A typed model name goes to the CLI exactly as typed (trimmed).
    fn model_name_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::ModelName(input)) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if input.key(key) != Edit::Submit {
            return vec![];
        }
        let name = input.text().trim().to_string();
        if name.is_empty() {
            self.message = Some("type a model name, or Esc to go back".into());
            return vec![];
        }
        self.overlays.pop();
        let effort = match self.overlays.pop() {
            Some(Overlay::Model(m)) => m.effort(),
            _ => None,
        };
        self.set_model(Some(name), effort);
        vec![]
    }

    /// Model and effort for the quick prompt below.
    fn set_model(&mut self, model: Option<String>, effort: Option<String>) {
        if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
            q.launch.model = model;
            q.launch.effort = effort;
        }
    }

    fn project_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Project(picker)) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if picker.key(key) == Pick::Chosen
            && let Some(id) = picker.selected().map(|p| p.id)
        {
            self.overlays.pop();
            if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
                q.project = id;
            }
        }
        vec![]
    }

    /// Why a card cannot take a typed instruction now: it is not running, or it waits
    /// on an answer (a pasted text and Enter would pick the highlighted choice).
    fn follow_up_refused(&self, id: SessionId) -> Option<&'static str> {
        match self.state.sessions.iter().find(|s| s.id == id) {
            Some(s) if s.status == AgentStatus::NeedsFeedback => {
                Some("waiting for an answer — Enter to open it")
            }
            Some(s) if s.status.is_live() => None,
            _ => Some("not running — Enter resumes"),
        }
    }

    /// `Space`: only a running card that is not waiting on an answer can take an
    /// instruction.
    fn open_follow_up(&mut self) {
        let Some(session) = self.selected else {
            return;
        };
        if let Some(why) = self.follow_up_refused(session) {
            self.message = Some(why.into());
            return;
        }
        self.overlays.push(Overlay::FollowUp {
            session,
            input: TextInput::new(false),
        });
    }

    /// Enter types the text into the card's agent and presses Enter. The text goes as
    /// a paste (bracketed when the agent asked for that), so a multi-line text arrives
    /// as one message and an agent that treats fast typing as a paste still submits.
    fn follow_up_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::FollowUp { session, input }) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if input.key(key) != Edit::Submit {
            return vec![];
        }
        let (session, text) = (*session, input.text().to_string());
        self.overlays.pop();
        if text.trim().is_empty() {
            return vec![];
        }
        if let Some(why) = self.follow_up_refused(session) {
            self.message = Some(why.into());
            return vec![];
        }
        let modes = self
            .screens
            .get(&session)
            .map(|s| s.modes)
            .unwrap_or_default();
        vec![
            Action::Send(ClientRequest::Input {
                session,
                data: encode_paste(&text, &modes),
            }),
            Action::Send(ClientRequest::Input {
                session,
                data: b"\r".to_vec(),
            }),
        ]
    }

    /// The list is fixed when it opens (attention order then); the rows show live status.
    fn open_palette(&mut self) {
        let sessions = self.visible_sessions();
        let labels: HashMap<SessionId, String> = sessions
            .iter()
            .map(|s| {
                let project = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id == s.project)
                    .map_or("", |p| p.name.as_str());
                (
                    s.id,
                    format!("{project} {} {}", s.display_name(), s.kind.label()),
                )
            })
            .collect();
        let order = attention_order(&sessions);
        self.overlays.push(Overlay::Palette(ListPicker::new(
            order,
            |id| labels.get(id).cloned().unwrap_or_default(),
            true,
        )));
    }

    /// Enter goes to the session: its project and card. Focus mode stays focus mode.
    fn palette_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Palette(picker)) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if picker.key(key) == Pick::Chosen
            && let Some(id) = picker.selected().copied()
        {
            self.overlays.pop();
            self.select(id);
        }
        vec![]
    }

    /// `o`: starts in the folder around the current project, or the home folder.
    fn open_project_browser(&mut self) -> Vec<Action> {
        let dir = self
            .project
            .and_then(|id| self.state.projects.iter().find(|p| p.id == id))
            .and_then(|p| p.path.parent().map(Path::to_path_buf))
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.overlays.push(Overlay::OpenProject(OpenProject::new(
            &self.state.projects,
            dir.clone(),
        )));
        vec![Action::ListDir(dir)]
    }

    /// A folder listing arrived; one for a folder the browser already left is dropped.
    pub fn listed(&mut self, dir: &Path, listing: Result<Listing, String>) {
        for o in &mut self.overlays {
            if let Overlay::OpenProject(open) = o
                && open.dir == dir
                && open.loading
            {
                open.listed(listing);
                return;
            }
        }
    }

    /// Enter opens a project or goes into a folder; Tab opens a folder as a project;
    /// → goes in, ← goes up.
    fn open_project_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::OpenProject(open)) = self.overlays.last_mut() else {
            return vec![];
        };
        let chosen = open.list.selected().cloned();
        match (key.code, chosen) {
            (KeyCode::Esc, _) => {
                self.overlays.pop();
            }
            (KeyCode::Left, _) => {
                if let Some(parent) = open.dir.parent().map(Path::to_path_buf) {
                    open.enter(parent.clone());
                    return vec![Action::ListDir(parent)];
                }
            }
            (KeyCode::Right | KeyCode::Enter, Some(BrowseEntry::Dir(d))) => {
                open.enter(d.path.clone());
                return vec![Action::ListDir(d.path)];
            }
            // The daemon resolves links in the path; the project it opens may show
            // another path, so one that was not open before counts too.
            (KeyCode::Tab, Some(BrowseEntry::Dir(d))) => {
                self.overlays.pop();
                let open_before = self.open_projects().map(|p| p.id).collect();
                self.project_pending = Some(ProjectPending::Path {
                    path: d.path.clone(),
                    open_before,
                });
                return vec![Action::Send(ClientRequest::AddProject { path: d.path })];
            }
            (KeyCode::Enter | KeyCode::Tab, Some(BrowseEntry::Project(p))) => {
                self.overlays.pop();
                if p.open {
                    self.project = Some(p.id);
                    self.selected = None;
                    self.repair_selection();
                    return vec![];
                }
                self.project_pending = Some(ProjectPending::Known(p.id));
                return vec![Action::Send(ClientRequest::OpenProject { project: p.id })];
            }
            _ => {
                open.list.key(key);
            }
        }
        vec![]
    }

    /// `r`: the box starts with the name the card shows.
    fn open_rename(&mut self) {
        let Some(info) = self.selected_info() else {
            return;
        };
        let (session, input) = (info.id, TextInput::with_text(info.display_name(), false));
        self.overlays.push(Overlay::Rename { session, input });
    }

    fn rename_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Rename { session, input }) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if input.key(key) != Edit::Submit {
            return vec![];
        }
        let name = input
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if name.is_empty() {
            self.message = Some("a name cannot be empty".into());
            return vec![];
        }
        let session = *session;
        self.overlays.pop();
        vec![Action::Send(ClientRequest::RenameSession { session, name })]
    }

    /// Enter on a card: focus a live session, resume a stopped one.
    fn enter(&mut self) -> Vec<Action> {
        let Some(info) = self.selected_info() else {
            return vec![];
        };
        if matches!(
            info.status,
            AgentStatus::Exited { .. } | AgentStatus::Disconnected
        ) {
            let id = info.id;
            if self.resume_pending == Some(id) {
                return vec![]; // already asked; wait for it to come back
            }
            let (cols, rows) = self.pane;
            self.resume_pending = Some(id);
            return vec![Action::Send(ClientRequest::Resume {
                session: id,
                cols: cols.max(20),
                rows: rows.max(5),
            })];
        }
        self.mode = Mode::Focus;
        vec![]
    }

    fn navigate(&mut self, c: char) {
        match c {
            '.' | ',' => {
                let visible = self.visible_sessions();
                if let Some(id) = next_in_attention(&visible, self.selected, c == '.') {
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

    fn set_archive_view(&mut self, on: bool) {
        self.archive_view = on;
        self.selected = None;
        self.card_scroll = 0;
        self.repair_selection();
    }

    /// The archive view: move, restore (Enter), delete (d), leave (A, Esc). Keys that
    /// start or reach live sessions do nothing here.
    fn archive_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.set_archive_view(false),
            KeyCode::Char('A') if !ctrl => self.set_archive_view(false),
            KeyCode::Enter => return self.restore(),
            KeyCode::Char('d') if ctrl => self.half_page(1),
            KeyCode::Char('u') if ctrl => self.half_page(-1),
            KeyCode::Char('d') => {
                if let Some(id) = self.selected {
                    self.mode = Mode::ConfirmKill(id);
                }
            }
            KeyCode::Char('q') => self.mode = Mode::ConfirmQuit,
            KeyCode::Char(']') => self.switch_project(1),
            KeyCode::Char('[') => self.switch_project(-1),
            KeyCode::Char(c @ ('h' | 'j' | 'k' | 'l')) => self.navigate(c),
            _ => {}
        }
        vec![]
    }

    /// Enter in the archive view: the card comes back to the grid and, if it is not
    /// running, resumes; it is focused once it is back.
    fn restore(&mut self) -> Vec<Action> {
        let Some(id) = self.selected else {
            return vec![];
        };
        self.archive_view = false;
        let mut actions = vec![Action::Send(ClientRequest::UnarchiveSession {
            session: id,
        })];
        actions.extend(self.enter());
        actions
    }

    /// Each frame's layout: cards per row and rows that fit. Scrolls just enough to
    /// keep the selected card on screen.
    pub fn set_card_window(&mut self, per_row: usize, rows: usize) {
        self.cards_per_row = per_row.max(1);
        self.card_rows = rows.max(1);
        let sessions = self.project_sessions();
        let total_rows = sessions.len().div_ceil(self.cards_per_row);
        if let Some(pos) = sessions.iter().position(|s| Some(s.id) == self.selected) {
            let row = pos / self.cards_per_row;
            if row < self.card_scroll {
                self.card_scroll = row;
            } else if row >= self.card_scroll + self.card_rows {
                self.card_scroll = row + 1 - self.card_rows;
            }
        }
        self.card_scroll = self
            .card_scroll
            .min(total_rows.saturating_sub(self.card_rows));
    }

    /// Ctrl+D / Ctrl+U: half a screen of cards down or up, selection and view together.
    fn half_page(&mut self, direction: isize) {
        let half = (self.card_rows / 2).max(1);
        self.move_by(direction * (half * self.cards_per_row) as isize);
        self.card_scroll = self
            .card_scroll
            .saturating_add_signed(direction * half as isize);
        let (per_row, rows) = (self.cards_per_row, self.card_rows);
        self.set_card_window(per_row, rows);
    }

    fn switch_project(&mut self, delta: isize) {
        let open: Vec<ProjectId> = self.open_projects().map(|p| p.id).collect();
        let n = open.len() as isize;
        if n == 0 {
            return;
        }
        let pos = self
            .project
            .and_then(|p| open.iter().position(|x| *x == p))
            .unwrap_or(0) as isize;
        self.project = Some(open[((pos + delta).rem_euclid(n)) as usize]);
        self.selected = None;
        self.repair_selection();
    }

    /// After `OpenProject` or `AddProject`, the state that has the project open brings
    /// its tab to the front.
    fn switch_to_pending_project(&mut self) {
        let found = match &self.project_pending {
            Some(ProjectPending::Known(id)) => {
                self.state.projects.iter().find(|p| p.id == *id && p.open)
            }
            Some(ProjectPending::Path { path, open_before }) => {
                let open = || self.open_projects();
                open()
                    .find(|p| p.path == *path)
                    .or_else(|| open().find(|p| !open_before.contains(&p.id)))
            }
            None => None,
        };
        if let Some(id) = found.map(|p| p.id) {
            self.project_pending = None;
            self.project = Some(id);
            self.selected = None;
        }
    }

    /// A card the user started or resumed is here: it takes the focus from the grid or
    /// the pane. Under a question it is only selected, so the question keeps its keys;
    /// the archive view keeps its own selection.
    fn arrived(&mut self, id: SessionId) {
        match self.mode {
            Mode::Grid if self.archive_view => {}
            Mode::Grid | Mode::Focus => {
                self.select(id);
                self.archive_view = false;
                self.mode = Mode::Focus;
            }
            _ => self.select(id),
        }
    }

    fn select(&mut self, id: SessionId) {
        if let Some(s) = self.state.sessions.iter().find(|s| s.id == id) {
            self.project = Some(s.project);
            self.selected = Some(id);
        }
    }

    /// Keeps `project` on an open project and `selected` on one of its sessions. A
    /// focused card that has to be left for another goes back to the grid.
    fn repair_selection(&mut self) {
        if !self
            .project
            .is_some_and(|p| self.open_projects().any(|x| x.id == p))
        {
            let first = self.open_projects().next().map(|p| p.id);
            self.project = first;
        }
        let valid = self
            .selected
            .is_some_and(|id| self.project_sessions().iter().any(|s| s.id == id));
        if !valid {
            self.selected = self.project_sessions().first().map(|s| s.id);
            if matches!(self.mode, Mode::Focus | Mode::FocusPrefix) {
                self.mode = Mode::Grid;
            }
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
            model: None,
            effort: None,
            user_named: false,
            archived: false,
        }
    }

    /// Two projects: "api" with three sessions, "web" with one waiting on the user.
    fn app() -> (App, Vec<SessionInfo>) {
        let api = ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: "/api".into(),
            open: true,
        };
        let web = ProjectInfo {
            id: ProjectId::new(),
            name: "web".into(),
            path: "/web".into(),
            open: true,
        };
        let s = vec![
            session(api.id, "a1", AgentStatus::Finished),
            session(api.id, "a2", AgentStatus::Unseen),
            session(api.id, "a3", AgentStatus::Running),
            session(web.id, "w1", AgentStatus::NeedsFeedback),
        ];
        let mut app = App::new();
        app.pane_resized(80, 20);
        app.set_card_window(2, 4);
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![api, web],
            sessions: s.clone(),
            ..StateSnapshot::default()
        }));
        (app, s)
    }

    /// The highlighted row of the harness picker, when it is the top overlay.
    fn picker(app: &App) -> Option<usize> {
        match app.overlays.last() {
            Some(Overlay::Harness(p)) => p.selected_index(),
            _ => None,
        }
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
            open: true,
        };
        let actions = app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s[..3].to_vec(),
            ..StateSnapshot::default()
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
        app.on_key(k(K::Char('n')));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::CreateSession {
                project: s[0].project,
                kind: SessionKind::Agent {
                    harness: Harness::Claude
                },
                prompt: None,
                model: None,
                effort: None,
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
    fn n_opens_the_picker_and_enter_starts_the_highlighted_cli() {
        let (mut app, s) = app();
        assert_eq!(
            sent(&app.on_key(k(K::Char('n')))),
            vec![&ClientRequest::RescanHarnesses],
            "a CLI installed since the daemon started shows up"
        );
        assert_eq!(picker(&app), Some(0));
        app.on_key(k(K::Char('k')));
        assert_eq!(picker(&app), Some(0), "stops at the top");
        app.on_key(k(K::Char('j')));
        assert_eq!(picker(&app), Some(1));
        let actions = app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::CreateSession {
                project: s[0].project,
                kind: SessionKind::Agent {
                    harness: Harness::Codex
                },
                prompt: None,
                model: None,
                effort: None,
                cols: 80,
                rows: 20
            }]
        );
    }

    // A card the user started arrives while a question is on screen: the question stays
    // (a `y` meant for it must not reach the new card), the card is only selected.
    #[test]
    fn a_new_card_arriving_under_a_question_does_not_take_the_focus() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('n')));
        app.on_key(k(K::Enter));
        app.on_key(k(K::Char('q')));
        assert_eq!(app.mode, Mode::ConfirmQuit);
        let fresh = session(s[0].project, "claude-5", AgentStatus::Fresh);
        app.on_event(ServerEvent::SessionUpdated(fresh.clone()));
        assert_eq!(app.mode, Mode::ConfirmQuit);
        assert_eq!(app.selected, Some(fresh.id));
        app.on_key(k(K::Esc));
        let later = session(s[0].project, "claude-6", AgentStatus::Fresh);
        app.on_event(ServerEvent::SessionUpdated(later));
        assert_eq!(
            (app.selected, app.mode),
            (Some(fresh.id), Mode::Grid),
            "the request was used up"
        );

        // a resumed card coming back while the archive is showing leaves it alone
        let (mut app, s) = self::app();
        let mut stopped = s[0].clone();
        stopped.status = AgentStatus::Disconnected;
        app.on_event(ServerEvent::SessionUpdated(stopped.clone()));
        app.on_key(k(K::Enter));
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[1],
            AgentStatus::Disconnected,
        )));
        app.on_key(k(K::Char('A')));
        stopped.status = AgentStatus::Fresh;
        app.on_event(ServerEvent::SessionUpdated(stopped));
        assert!(app.archive_view);
        assert_eq!((app.selected, app.mode), (Some(s[1].id), Mode::Grid));
    }

    #[test]
    fn a_cli_that_is_not_installed_cannot_be_started() {
        let (mut app, _) = app();
        app.on_event(ServerEvent::Harnesses(vec![
            HarnessInfo {
                harness: Harness::Claude,
                available: false,
            },
            HarnessInfo {
                harness: Harness::Codex,
                available: true,
            },
            HarnessInfo {
                harness: Harness::OpenCode,
                available: false,
            },
        ]));
        app.on_key(k(K::Char('n')));
        assert_eq!(picker(&app), Some(1), "opens on the first available CLI");
        let actions = app.on_key(k(K::Char('3')));
        assert!(sent(&actions).is_empty());
        assert!(app.overlays.is_empty());
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("opencode is not installed")
        );
    }

    #[test]
    fn escape_closes_the_picker_without_sending() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('n')));
        assert!(app.on_key(k(K::Esc)).is_empty());
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::Grid);
    }

    #[test]
    fn keys_go_to_the_top_overlay_and_esc_closes_only_that_one() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('n')));
        app.on_key(k(K::Char('l')));
        assert_eq!(app.selected, Some(s[0].id), "l did not move the grid");
        let below = app.overlays[0].clone();
        app.overlays.push(below);
        app.on_key(k(K::Esc));
        assert_eq!(app.overlays.len(), 1);
    }

    #[test]
    fn ctrl_q_closes_every_overlay_and_leaves_focus_mode() {
        let (mut app, _) = app();
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        app.overlays
            .push(Overlay::Harness(overlay::harness_picker(&app.harnesses)));
        let again = app.overlays[0].clone();
        app.overlays.push(again);
        assert!(sent(&app.on_key(ctrl('q'))).is_empty());
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::Grid);
    }

    #[test]
    fn a_newer_harness_list_updates_the_open_picker() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('n')));
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Codex,
            available: true,
        }]));
        match app.overlays.last() {
            Some(Overlay::Harness(p)) => assert_eq!(p.items().len(), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn enter_on_a_stopped_card_resumes_it_and_focuses_when_it_is_back() {
        let (mut app, s) = app();
        let mut stopped = s[0].clone();
        stopped.status = AgentStatus::Disconnected;
        app.on_event(ServerEvent::SessionUpdated(stopped.clone()));
        assert_eq!(app.selected, Some(stopped.id));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::Resume {
                session: stopped.id,
                cols: 80,
                rows: 20
            }]
        );
        assert_eq!(
            app.mode,
            Mode::Grid,
            "not focused until the process is back"
        );
        let mut back = stopped.clone();
        back.status = AgentStatus::Fresh;
        let actions = app.on_event(ServerEvent::SessionUpdated(back));
        assert_eq!(app.mode, Mode::Focus);
        assert!(
            sent(&actions).contains(&&ClientRequest::Attach {
                session: stopped.id,
                cols: 80,
                rows: 20
            }),
            "re-attaches to the new process"
        );
    }

    #[test]
    fn a_second_enter_while_resuming_sends_nothing() {
        let (mut app, s) = app();
        let mut stopped = s[0].clone();
        stopped.status = AgentStatus::Exited { code: Some(1) };
        app.on_event(ServerEvent::SessionUpdated(stopped.clone()));
        assert_eq!(sent(&app.on_key(k(K::Enter))).len(), 1);
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        // an error ends the wait: Enter resumes again
        app.on_event(ServerEvent::Error {
            message: "no".into(),
        });
        assert_eq!(sent(&app.on_key(k(K::Enter))).len(), 1);
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
        assert_eq!(app.mode, Mode::Grid, "C-q always escapes");
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

    // Another client closes the project of the focused card: the pane must not go on
    // typing into whichever card the selection lands on.
    #[test]
    fn closing_the_focused_cards_project_elsewhere_returns_to_the_grid() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        let mut state = app.state.clone();
        state.projects[0].open = false;
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.selected, Some(s[3].id));
        assert_eq!(app.mode, Mode::Grid);
    }

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
            open: true,
        };
        let actions = fresh.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s[..1].to_vec(),
            ..StateSnapshot::default()
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

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(k(K::Char(c)));
        }
    }

    fn quick_prompt(app: &App) -> &QuickPrompt {
        app.overlays
            .iter()
            .rev()
            .find_map(|o| match o {
                Overlay::QuickPrompt(q) => Some(q),
                _ => None,
            })
            .expect("a quick prompt is open")
    }

    fn launch(harness: Harness, model: Option<&str>, effort: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            harness,
            model: model.map(Into::into),
            effort: effort.map(Into::into),
        }
    }

    #[test]
    fn p_opens_the_quick_prompt_on_the_last_launch_and_asks_for_the_history() {
        let (mut app, s) = app();
        app.state.last_launch = Some(launch(Harness::Codex, Some("gpt-5"), Some("high")));
        let actions = app.on_key(k(K::Char('p')));
        assert_eq!(
            sent(&actions),
            vec![
                &ClientRequest::ListPromptHistory { limit: 50 },
                &ClientRequest::RescanHarnesses
            ]
        );
        let q = quick_prompt(&app);
        assert_eq!(q.project, s[0].project);
        assert_eq!(
            q.launch,
            launch(Harness::Codex, Some("gpt-5"), Some("high"))
        );
    }

    #[test]
    fn enter_starts_the_task_with_its_prompt_model_and_effort_and_focuses_it() {
        let (mut app, s) = app();
        app.state.last_launch = Some(launch(Harness::Claude, Some("opus"), Some("max")));
        app.on_key(k(K::Char('p')));
        type_text(&mut app, "fix the login redirect");
        app.on_key(KeyEvent::new(K::Enter, M::SHIFT));
        type_text(&mut app, "and add a test");
        let actions = app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        assert_eq!(
            sent(&actions),
            vec![
                &ClientRequest::SetLastLaunch(launch(Harness::Claude, Some("opus"), Some("max"))),
                &ClientRequest::CreateSession {
                    project: s[0].project,
                    kind: SessionKind::Agent {
                        harness: Harness::Claude
                    },
                    prompt: Some("fix the login redirect\nand add a test".into()),
                    model: Some("opus".into()),
                    effort: Some("max".into()),
                    cols: 80,
                    rows: 20
                }
            ]
        );
        let fresh = session(s[0].project, "claude-5", AgentStatus::Fresh);
        app.on_event(ServerEvent::SessionUpdated(fresh.clone()));
        assert_eq!((app.selected, app.mode), (Some(fresh.id), Mode::Focus));
    }

    #[test]
    fn an_empty_quick_prompt_starts_the_cli_bare() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        type_text(&mut app, "   ");
        let actions = app.on_key(k(K::Enter));
        match sent(&actions)[1] {
            ClientRequest::CreateSession { prompt, model, .. } => {
                assert_eq!((prompt, model), (&None, &None));
            }
            other => panic!("{other:?}"),
        }
    }

    // Every picker opened from the quick prompt, chosen from or cancelled, returns to
    // the same text.
    #[test]
    fn the_prompt_text_survives_opening_and_cancelling_every_picker() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        type_text(&mut app, "fix the bug");
        app.on_key(KeyEvent::new(K::Enter, M::ALT));
        type_text(&mut app, "and test it");
        let text = quick_prompt(&app).input.text().to_string();
        let before = quick_prompt(&app).launch.clone();
        let keys = [
            vec![k(K::Tab), k(K::Esc)],
            vec![ctrl('o'), k(K::Esc)],
            vec![
                ctrl('o'),
                k(K::Char('j')),
                k(K::Enter),
                k(K::Char('x')),
                k(K::Esc),
                k(K::Esc),
            ],
            vec![ctrl('p'), k(K::Char('w')), k(K::Esc)],
        ];
        for sequence in keys {
            for key in sequence {
                app.on_key(key);
            }
            assert_eq!(app.overlays.len(), 1, "back to the quick prompt");
            assert_eq!(quick_prompt(&app).input.text(), text);
            assert_eq!(quick_prompt(&app).launch, before);
        }
        app.on_key(k(K::Tab));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Enter));
        assert_eq!(quick_prompt(&app).input.text(), text, "and after choosing");
        assert_eq!(quick_prompt(&app).launch.harness, Harness::Codex);
    }

    #[test]
    fn tab_switches_the_cli_and_drops_a_model_that_belongs_to_the_old_one() {
        let (mut app, _) = app();
        app.state.last_launch = Some(launch(Harness::Claude, Some("opus"), Some("high")));
        app.on_key(k(K::Char('p')));
        app.on_key(k(K::Tab));
        app.on_key(k(K::Char('2')));
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::Codex, None, Some("high"))
        );
    }

    #[test]
    fn a_cli_that_is_not_installed_is_not_chosen_or_started() {
        let (mut app, _) = app();
        app.state.last_launch = Some(launch(Harness::Codex, None, None));
        app.on_event(ServerEvent::Harnesses(vec![
            HarnessInfo {
                harness: Harness::Claude,
                available: false,
            },
            HarnessInfo {
                harness: Harness::Codex,
                available: false,
            },
            HarnessInfo {
                harness: Harness::OpenCode,
                available: true,
            },
        ]));
        app.on_key(k(K::Char('p')));
        assert_eq!(
            quick_prompt(&app).launch.harness,
            Harness::OpenCode,
            "the first installed one"
        );
        app.on_key(k(K::Tab));
        app.on_key(k(K::Char('1')));
        assert_eq!(quick_prompt(&app).launch.harness, Harness::OpenCode);
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("claude is not installed")
        );
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::OpenCode,
            available: false,
        }]));
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        assert_eq!(app.overlays.len(), 1, "the prompt stays open");
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("opencode is not installed")
        );
    }

    #[test]
    fn ctrl_o_picks_a_recent_model_and_an_effort() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        assert_eq!(
            sent(&app.on_key(ctrl('o'))),
            vec![&ClientRequest::ListModels {
                harness: Harness::Claude
            }]
        );
        app.on_event(ServerEvent::Models {
            harness: Harness::Claude,
            recent: vec!["opus".into(), "sonnet".into()],
        });
        for key in [
            K::Char('j'),
            K::Char('j'),
            K::Char('l'),
            K::Char('l'),
            K::Right,
            K::Enter,
        ] {
            app.on_key(k(key));
        }
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::Claude, Some("sonnet"), Some("high"))
        );
        app.on_key(ctrl('o'));
        for key in [
            K::Char('k'),
            K::Char('k'),
            K::Char('h'),
            K::Char('h'),
            K::Char('h'),
            K::Enter,
        ] {
            app.on_key(k(key));
        }
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::Claude, None, None)
        );
    }

    #[test]
    fn a_typed_model_name_is_used_exactly_as_typed() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        app.on_key(ctrl('o'));
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Enter));
        assert!(matches!(app.overlays.last(), Some(Overlay::ModelName(_))));
        app.on_key(k(K::Enter));
        assert!(app.message.is_some(), "an empty name is refused");
        type_text(&mut app, r#" my "odd" model "#);
        app.on_key(k(K::Enter));
        assert_eq!(app.overlays.len(), 1);
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::Claude, Some(r#"my "odd" model"#), Some("low"))
        );
    }

    #[test]
    fn opencode_has_no_effort_to_pick() {
        let (mut app, _) = app();
        app.state.last_launch = Some(launch(Harness::OpenCode, None, None));
        app.on_key(k(K::Char('p')));
        app.on_key(ctrl('o'));
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Enter));
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::OpenCode, None, None)
        );
    }

    #[test]
    fn ctrl_p_starts_the_task_in_another_project() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('p')));
        app.on_key(ctrl('p'));
        type_text(&mut app, "web");
        app.on_key(k(K::Enter));
        assert_eq!(quick_prompt(&app).project, s[3].project);
        let actions = app.on_key(k(K::Enter));
        assert!(matches!(
            sent(&actions)[1],
            ClientRequest::CreateSession { project, .. } if *project == s[3].project
        ));
    }

    #[test]
    fn up_in_the_empty_prompt_brings_back_the_last_prompt() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        app.on_event(ServerEvent::PromptHistory(vec![
            "the last one".into(),
            "older".into(),
        ]));
        app.on_key(k(K::Up));
        assert_eq!(quick_prompt(&app).input.text(), "the last one");
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('p')));
        app.on_key(k(K::Up));
        assert_eq!(
            quick_prompt(&app).input.text(),
            "the last one",
            "remembered for the next prompt"
        );
    }

    #[test]
    fn a_paste_goes_into_the_open_text_box_not_the_pane() {
        let (mut app, _) = app();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('p')));
        assert_eq!(app.mode, Mode::Focus);
        assert!(app.on_paste("line one\nline two").is_empty());
        assert_eq!(quick_prompt(&app).input.text(), "line one\nline two");
        app.on_key(k(K::Esc));
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::Focus, "Esc returns to the focused pane");
    }

    #[test]
    fn space_types_one_instruction_into_the_card_and_presses_enter() {
        let (mut app, s) = app();
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "run the tests");
        let actions = app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::Grid, "the grid stays");
        assert_eq!(
            sent(&actions),
            vec![
                &ClientRequest::Input {
                    session: s[0].id,
                    data: b"run the tests".to_vec()
                },
                &ClientRequest::Input {
                    session: s[0].id,
                    data: b"\r".to_vec()
                },
            ]
        );
    }

    #[test]
    fn a_multi_line_follow_up_arrives_as_one_pasted_message() {
        let (mut app, s) = app();
        let mut screen = Snapshot::blank(10, 2);
        screen.modes.bracketed_paste = true;
        app.screens.insert(s[0].id, screen);
        app.on_key(k(K::Char(' ')));
        app.on_paste("first line\nsecond line");
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions)[0],
            &ClientRequest::Input {
                session: s[0].id,
                data: b"\x1b[200~first line\nsecond line\x1b[201~".to_vec()
            }
        );
    }

    // A follow-up must never reach a card whose agent is not running, not even one that
    // stopped while the box was open.
    #[test]
    fn a_follow_up_to_a_card_that_is_not_running_sends_nothing() {
        let (mut app, s) = app();
        let mut stopped = s[0].clone();
        stopped.status = AgentStatus::Disconnected;
        app.on_event(ServerEvent::SessionUpdated(stopped.clone()));
        assert!(sent(&app.on_key(k(K::Char(' ')))).is_empty());
        assert!(app.overlays.is_empty());
        assert_eq!(app.message.as_deref(), Some("not running — Enter resumes"));

        let (mut app, s) = self::app();
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "too late");
        let mut exited = s[0].clone();
        exited.status = AgentStatus::Exited { code: Some(0) };
        app.on_event(ServerEvent::SessionUpdated(exited));
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        assert_eq!(app.message.as_deref(), Some("not running — Enter resumes"));
    }

    // A card waiting on an approval has a select open: a pasted text and Enter would pick
    // its highlighted answer. The answer is given in the card itself.
    #[test]
    fn a_follow_up_to_a_card_waiting_for_an_answer_sends_nothing() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('.')));
        assert_eq!(app.selected, Some(s[3].id));
        assert!(sent(&app.on_key(k(K::Char(' ')))).is_empty());
        assert!(app.overlays.is_empty());
        assert_eq!(
            app.message.as_deref(),
            Some("waiting for an answer — Enter to open it")
        );

        let (mut app, s) = self::app();
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "no, use pnpm instead");
        let mut asking = s[0].clone();
        asking.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(asking));
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        assert_eq!(
            app.message.as_deref(),
            Some("waiting for an answer — Enter to open it")
        );
    }

    #[test]
    fn a_first_prompt_can_be_typed_into_a_fresh_card() {
        let (mut app, s) = app();
        let mut fresh = s[0].clone();
        fresh.status = AgentStatus::Fresh;
        app.on_event(ServerEvent::SessionUpdated(fresh));
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "hello");
        assert_eq!(sent(&app.on_key(k(K::Enter))).len(), 2);
    }

    #[test]
    fn an_empty_follow_up_sends_nothing() {
        let (mut app, _) = app();
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "  ");
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn r_renames_starting_from_the_name_the_card_shows() {
        let (mut app, s) = app();
        let mut titled = s[0].clone();
        titled.title = Some("Fix Login".into());
        app.on_event(ServerEvent::SessionUpdated(titled));
        app.on_key(k(K::Char('r')));
        match app.overlays.last() {
            Some(Overlay::Rename { input, .. }) => assert_eq!(input.text(), "Fix Login"),
            other => panic!("{other:?}"),
        }
        app.on_key(ctrl('u'));
        type_text(&mut app, "login bug");
        assert_eq!(
            sent(&app.on_key(k(K::Enter))),
            vec![&ClientRequest::RenameSession {
                session: s[0].id,
                name: "login bug".into()
            }]
        );
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn an_empty_name_is_refused_and_the_box_stays_open() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('r')));
        app.on_key(ctrl('u'));
        assert!(sent(&app.on_key(k(K::Enter))).is_empty());
        assert_eq!(app.message.as_deref(), Some("a name cannot be empty"));
        assert_eq!(app.overlays.len(), 1);
    }

    fn palette(app: &App) -> Vec<SessionId> {
        match app.overlays.last() {
            Some(Overlay::Palette(p)) => p.visible().map(|(_, id, _)| *id).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn slash_lists_the_sessions_by_attention_and_enter_goes_there() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('/')));
        assert_eq!(palette(&app), vec![s[3].id, s[2].id, s[1].id, s[0].id]);
        app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        assert_eq!(
            (app.project, app.selected),
            (Some(s[3].project), Some(s[3].id))
        );
        assert_eq!(app.mode, Mode::Grid);
    }

    #[test]
    fn typing_narrows_the_palette_by_project_and_name() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('/')));
        type_text(&mut app, "web");
        assert_eq!(palette(&app), vec![s[3].id]);
        app.on_key(k(K::Backspace));
        app.on_key(k(K::Backspace));
        app.on_key(k(K::Backspace));
        type_text(&mut app, "a2");
        assert_eq!(palette(&app), vec![s[1].id]);
    }

    // The label is "project name kind": letters scattered over it must not match.
    #[test]
    fn a_palette_word_matches_only_where_it_appears_whole() {
        let orbit = ProjectInfo {
            id: ProjectId::new(),
            name: "orbit-api".into(),
            path: "/orbit-api".into(),
            open: true,
        };
        let s = vec![
            session(orbit.id, "limit uploads", AgentStatus::Running),
            session(orbit.id, "billing-fix", AgentStatus::Running),
        ];
        let mut app = App::new();
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![orbit],
            sessions: s.clone(),
            ..StateSnapshot::default()
        }));
        app.on_key(k(K::Char('/')));
        type_text(&mut app, "bill");
        assert_eq!(palette(&app), vec![s[1].id]);
    }

    #[test]
    fn the_palette_from_focus_mode_keeps_you_focused_on_the_new_card() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('/')));
        app.on_key(k(K::Down));
        let actions = app.on_key(k(K::Enter));
        assert_eq!((app.selected, app.mode), (Some(s[2].id), Mode::Focus));
        assert!(sent(&actions).contains(&&ClientRequest::Attach {
            session: s[2].id,
            cols: 80,
            rows: 20
        }));
    }

    // An archived card whose hooks keep coming (it may still be finishing) and the
    // sessions of a closed project are out of reach of the palette and of `.` / `,`.
    #[test]
    fn archived_and_closed_sessions_are_left_out_of_the_palette_and_the_attention_order() {
        let (mut app, s) = app();
        let mut archived = s[2].clone();
        archived.archived = true;
        archived.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(archived));
        let mut state = app.state.clone();
        state.projects[1].open = false;
        app.on_event(ServerEvent::State(state));
        app.on_key(k(K::Char('/')));
        assert_eq!(palette(&app), vec![s[1].id, s[0].id]);
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('.')));
        assert_eq!(
            app.selected,
            Some(s[1].id),
            "the unread card, not the archived one"
        );
        app.on_key(k(K::Char('.')));
        assert_eq!(app.selected, Some(s[0].id));
        app.on_key(k(K::Char('.')));
        assert_eq!(
            app.selected,
            Some(s[1].id),
            "and never the closed project's"
        );
    }

    fn browser(app: &App) -> &OpenProject {
        match app.overlays.last() {
            Some(Overlay::OpenProject(open)) => open,
            other => panic!("{other:?}"),
        }
    }

    fn listing(names: &[(&str, bool)], under: &Path) -> Result<Listing, String> {
        Ok(Listing {
            entries: names
                .iter()
                .map(|(name, git)| crate::browse::DirEntry {
                    name: name.to_string(),
                    path: under.join(name),
                    git: *git,
                })
                .collect(),
            truncated: false,
        })
    }

    fn close_web(app: &mut App) {
        let mut state = app.state.clone();
        state.projects[1].open = false;
        app.on_event(ServerEvent::State(state));
    }

    #[test]
    fn o_lists_closed_projects_first_then_the_folders_around_this_one() {
        let (mut app, _) = app();
        close_web(&mut app);
        assert_eq!(
            app.on_key(k(K::Char('o'))),
            vec![Action::ListDir(PathBuf::from("/"))]
        );
        assert!(browser(&app).loading);
        app.listed(
            Path::new("/"),
            listing(&[("api", true), ("notes", false)], Path::new("/")),
        );
        let labels: Vec<String> = browser(&app)
            .list
            .visible()
            .map(|(_, e, _)| e.label())
            .collect();
        assert_eq!(labels, ["web", "api", "api", "notes"]);
        assert!(!browser(&app).loading);
    }

    #[test]
    fn enter_on_a_closed_project_opens_it_and_brings_it_to_the_front() {
        let (mut app, s) = app();
        close_web(&mut app);
        app.on_key(k(K::Char('o')));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::OpenProject {
                project: s[3].project
            }]
        );
        assert!(app.overlays.is_empty());
        let mut state = app.state.clone();
        state.projects[1].open = true;
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.project, Some(s[3].project));
    }

    #[test]
    fn tab_on_a_folder_adds_it_as_a_project_and_brings_it_to_the_front() {
        let (mut app, _) = app();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("orbit")).unwrap();
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), listing(&[("orbit", true)], &root));
        type_text(&mut app, "orb");
        let actions = app.on_key(k(K::Tab));
        let path = root.join("orbit");
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::AddProject { path: path.clone() }]
        );
        let mut state = app.state.clone();
        let added = ProjectInfo {
            id: ProjectId::new(),
            name: "orbit".into(),
            path,
            open: true,
        };
        state.projects.push(added.clone());
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.project, Some(added.id));
    }

    // The path goes to the daemon as shown (it resolves links itself, off the UI
    // thread); the project it adds under its real path still comes to the front.
    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_link_is_added_as_shown_and_still_comes_to_the_front() {
        let (mut app, _) = app();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), listing(&[("link", false)], &root));
        type_text(&mut app, "link");
        let actions = app.on_key(k(K::Tab));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::AddProject {
                path: root.join("link")
            }]
        );
        let mut state = app.state.clone();
        let added = ProjectInfo {
            id: ProjectId::new(),
            name: "real".into(),
            path: root.join("real"),
            open: true,
        };
        state.projects.push(added.clone());
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.project, Some(added.id));
    }

    #[test]
    fn right_goes_into_a_folder_and_left_comes_back_up() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), listing(&[("code", false)], Path::new("/")));
        type_text(&mut app, "code");
        assert_eq!(
            app.on_key(k(K::Right)),
            vec![Action::ListDir(PathBuf::from("/code"))]
        );
        assert_eq!(browser(&app).dir, PathBuf::from("/code"));
        assert_eq!(
            browser(&app).list.query(),
            Some(""),
            "a new folder, a new filter"
        );
        assert_eq!(
            app.on_key(k(K::Left)),
            vec![Action::ListDir(PathBuf::from("/"))]
        );
    }

    // Listing happens off the UI thread: the browser answers keys while it waits, and
    // an answer for a folder it already left (or after it closed) changes nothing.
    #[test]
    fn a_slow_or_failed_listing_never_gets_in_the_way() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), listing(&[("code", false)], Path::new("/")));
        type_text(&mut app, "code");
        app.on_key(k(K::Right));
        app.listed(Path::new("/"), listing(&[("stale", false)], Path::new("/")));
        assert!(
            browser(&app)
                .list
                .items()
                .iter()
                .all(|e| e.label() != "stale"),
            "an answer for a folder already left"
        );
        assert!(browser(&app).loading, "still waiting for /code");
        app.on_key(k(K::Esc));
        app.listed(
            Path::new("/code"),
            listing(&[("late", false)], Path::new("/code")),
        );
        assert!(app.overlays.is_empty());
        app.on_key(k(K::Char('o')));
        app.listed(
            Path::new("/"),
            Err("cannot read /: permission denied".into()),
        );
        assert_eq!(
            browser(&app).error.as_deref(),
            Some("cannot read /: permission denied")
        );
        assert_eq!(
            browser(&app).list.items().len(),
            2,
            "the projects are still there"
        );
    }

    #[test]
    fn x_closes_the_project_tab_after_asking() {
        let (mut app, s) = app();
        assert!(sent(&app.on_key(k(K::Char('x')))).is_empty());
        assert_eq!(app.mode, Mode::ConfirmClose(s[0].project));
        assert_eq!(
            sent(&app.on_key(k(K::Char('y')))),
            vec![&ClientRequest::CloseProject {
                project: s[0].project
            }]
        );
        let mut state = app.state.clone();
        state.projects[0].open = false;
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.project, Some(s[3].project), "the next open tab");
        app.on_key(k(K::Char('x')));
        app.on_key(k(K::Char('n')));
        assert_eq!(app.mode, Mode::Grid, "any other key cancels");
    }

    #[test]
    fn tabs_and_their_keys_skip_closed_projects() {
        let (mut app, s) = app();
        let mut state = app.state.clone();
        state.projects[0].open = false;
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.project, Some(s[3].project));
        app.on_key(k(K::Char(']')));
        assert_eq!(app.project, Some(s[3].project));
        app.on_key(k(K::Char('2')));
        assert_eq!(app.project, Some(s[3].project), "there is no second tab");
    }

    /// One project with `n` shell sessions, two cards per row, `rows` rows on screen.
    fn many(n: usize, rows: usize) -> (App, Vec<SessionInfo>) {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: "/api".into(),
            open: true,
        };
        let s: Vec<SessionInfo> = (0..n)
            .map(|i| session(p.id, &format!("s{i}"), AgentStatus::Finished))
            .collect();
        let mut app = App::new();
        app.pane_resized(80, 20);
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s.clone(),
            ..StateSnapshot::default()
        }));
        app.set_card_window(2, rows);
        (app, s)
    }

    #[test]
    fn the_view_scrolls_to_keep_the_selected_card_on_screen() {
        let (mut app, s) = many(10, 2);
        for _ in 0..3 {
            app.on_key(k(K::Char('j')));
        }
        app.set_card_window(2, 2);
        assert_eq!(app.selected, Some(s[6].id));
        assert_eq!(app.card_scroll, 2, "row 3 is the last row on screen");
        for _ in 0..3 {
            app.on_key(k(K::Char('k')));
        }
        app.set_card_window(2, 2);
        assert_eq!(app.card_scroll, 0);
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_screen_of_cards() {
        let (mut app, s) = many(20, 4);
        app.on_key(ctrl('d'));
        assert_eq!(app.selected, Some(s[4].id), "two rows of two cards down");
        assert_eq!(app.card_scroll, 2);
        for _ in 0..5 {
            app.on_key(ctrl('d'));
        }
        assert_eq!(app.selected, Some(s[19].id), "stops at the last card");
        assert_eq!(app.card_scroll, 6, "the last rows fill the screen");
        app.on_key(ctrl('u'));
        assert_eq!(app.selected, Some(s[15].id));
        assert_eq!(app.card_scroll, 4);
        assert_eq!(app.mode, Mode::Grid, "Ctrl+D is not d (kill)");
    }

    fn archived(s: &SessionInfo, status: AgentStatus) -> SessionInfo {
        let mut a = s.clone();
        a.archived = true;
        a.status = status;
        a
    }

    #[test]
    fn a_archives_the_selected_card_after_asking() {
        let (mut app, s) = app();
        assert!(sent(&app.on_key(k(K::Char('a')))).is_empty());
        assert_eq!(app.mode, Mode::ConfirmArchive(s[0].id));
        assert_eq!(
            sent(&app.on_key(k(K::Char('y')))),
            vec![&ClientRequest::ArchiveSession { session: s[0].id }]
        );
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[0],
            AgentStatus::Finished,
        )));
        assert!(app.project_sessions().iter().all(|x| x.id != s[0].id));
        assert_eq!(app.selected, Some(s[1].id));
        app.on_key(k(K::Char('a')));
        app.on_key(k(K::Esc));
        assert_eq!(app.mode, Mode::Grid, "any other key cancels");
    }

    // An archived card's late hooks move its status, but it stays out of the grid and
    // never takes the selection or the focus.
    #[test]
    fn an_archived_card_stays_hidden_whatever_its_hooks_say() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Char('l')));
        app.on_key(k(K::Enter));
        assert_eq!((app.selected, app.mode), (Some(s[2].id), Mode::Focus));
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[2],
            AgentStatus::Running,
        )));
        assert_eq!(app.mode, Mode::Grid, "archived elsewhere: back to the grid");
        for status in [AgentStatus::NeedsFeedback, AgentStatus::Unseen] {
            app.on_event(ServerEvent::SessionUpdated(archived(&s[2], status)));
            assert!(app.project_sessions().iter().all(|x| x.id != s[2].id));
            assert_ne!(app.selected, Some(s[2].id));
        }
    }

    #[test]
    fn the_archive_view_shows_archived_cards_and_enter_restores_and_resumes() {
        let (mut app, s) = app();
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[1],
            AgentStatus::Disconnected,
        )));
        app.on_key(k(K::Char('A')));
        assert!(app.archive_view);
        let shown: Vec<SessionId> = app.project_sessions().iter().map(|x| x.id).collect();
        assert_eq!(shown, vec![s[1].id]);
        assert_eq!(app.selected, Some(s[1].id));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions)[..2],
            [
                &ClientRequest::UnarchiveSession { session: s[1].id },
                &ClientRequest::Resume {
                    session: s[1].id,
                    cols: 80,
                    rows: 20
                }
            ]
        );
        assert!(!app.archive_view);
        let mut back = s[1].clone();
        back.status = AgentStatus::Disconnected;
        app.on_event(ServerEvent::SessionUpdated(back.clone()));
        back.status = AgentStatus::Fresh;
        app.on_event(ServerEvent::SessionUpdated(back));
        assert_eq!((app.selected, app.mode), (Some(s[1].id), Mode::Focus));
    }

    // Ctrl+A is a prefix habit; with Ctrl held, the letter keys of the grid do nothing.
    #[test]
    fn ctrl_with_a_grid_letter_does_nothing() {
        let (mut app, _) = app();
        for c in ['a', 'p', 'r', 'o', 'x', '/', ' '] {
            assert!(sent(&app.on_key(ctrl(c))).is_empty(), "{c:?}");
            assert!(app.overlays.is_empty(), "{c:?}");
            assert_eq!(app.mode, Mode::Grid, "{c:?}");
        }
        let ctrl_shift_a = KeyEvent::new(K::Char('A'), M::CONTROL | M::SHIFT);
        app.on_key(ctrl_shift_a);
        assert!(!app.archive_view);
        app.on_key(k(K::Char('A')));
        app.on_key(ctrl_shift_a);
        assert!(app.archive_view, "and it does not leave the archive view");
    }

    #[test]
    fn the_archive_view_only_moves_restores_deletes_and_leaves() {
        let (mut app, s) = app();
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[1],
            AgentStatus::Disconnected,
        )));
        app.on_key(k(K::Char('A')));
        for c in ['p', 'n', ' ', 'r', 'a', '/', 'o', 't', '.'] {
            assert!(sent(&app.on_key(k(K::Char(c)))).is_empty(), "{c:?}");
            assert!(app.overlays.is_empty(), "{c:?}");
            assert_eq!(app.mode, Mode::Grid, "{c:?}");
        }
        app.on_key(k(K::Char('d')));
        assert_eq!(app.mode, Mode::ConfirmKill(s[1].id));
        app.on_key(k(K::Esc));
        app.on_key(k(K::Esc));
        assert!(!app.archive_view);
        assert_eq!(app.selected, Some(s[0].id));
    }
}
