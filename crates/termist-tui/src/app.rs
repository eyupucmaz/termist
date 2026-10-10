use crate::bands::Slot;
use crate::browse::Listing;
use crate::copy::{Copy, CopyOut};
use crate::diff::DiffAction;
use crate::diff::local as mirror;
use crate::encode::{encode_key, encode_paste, encode_wheel};
use crate::finder::{FindKind, Finder};
use crate::keys::{Action as KeyAction, Context, KeySpec, Keymap};
use crate::list_picker::{ListPicker, Pick};
use crate::overlay::{
    self, BrowseEntry, Capture, CaptureTarget, HandTo, ModelChoice, ModelPicker, OpenProject,
    Overlay, QuickPrompt, SETTING_ROWS, SettingRow, SettingsView, key_rows,
};
use crate::prs::compose::{Compose, ComposeAction, Sending, Target};
use crate::prs::issues::{IssueAction, ProjectIssues};
use crate::prs::{self, Ask, Mine, PrAction, PrLayout, PrView, ProjectPrs, Section, Subject};
use crate::scene_view::{self, ShowKind, Showing};
use crate::selection::Selection;
use crate::settings::ConfigEdit;
use crate::text_input::{Edit, TextInput};
use crate::theme::{Theme, Themes};
use crate::toast::{self, Toast, ToastKind, Toasts};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use termist_core::config::Preset;
use termist_core::config::{ColorDepth, Config, PanePosition, Sound};
use termist_core::github::{CommentKind, GhState, PrDetail, PrDiff, PrRef, PrWrite, RepoInfo};
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, LaunchOptions, ModelInfo, ProjectId,
    ProjectInfo, ServerEvent, SessionId, SessionInfo, SessionKind, Snapshot, StateSnapshot,
    attention_order, next_in_attention,
};
use termist_core::{DiffMode, ReadState, Scroll, TermColors};
use termist_scenes::{Scene, TimeOfDay};

/// What ←/→ steps through for the idle screen, in minutes; 0 is off.
const IDLE_CHOICES: [u32; 6] = [0, 5, 10, 15, 30, 60];

/// How long the splash stays at most.
const SPLASH: Duration = Duration::from_secs(1);

/// How many earlier prompts the quick prompt asks for.
const PROMPT_HISTORY: u32 = 50;

/// Lines one wheel notch scrolls.
const WHEEL_LINES: u32 = 3;

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
    /// `D` was pressed on a comment of yours (`App::deleting`); `y` / Enter deletes it.
    ConfirmDelete,
    /// `X` was pressed on a worktree (`App::removing`); `y` / Enter removes it. `files`
    /// it has uncommitted changes in, once the daemon said so (then they go too).
    ConfirmRemove {
        files: u32,
    },
}

/// What the body shows: the grid of cards, the archived cards, or pull requests.
// One View lives in the App; boxing the PR view would only add a pointer to every key.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Grid,
    /// `A`: the project's archived cards instead of its live ones.
    Archive,
    /// `v`: the project's pull requests.
    Prs(PrView),
    /// `g`: a folder's diff (Ayna).
    Diff(crate::diff::local::LocalView),
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Send(ClientRequest),
    /// List a folder off the UI thread; the result comes back through `App::listed`.
    ListDir(PathBuf),
    /// Save a settings change to config.toml.
    WriteConfig(ConfigEdit),
    /// An agent started waiting or finished: a sound, and a desktop notification when
    /// the terminal is not in front.
    Alert(Alert),
    /// A sound just chosen in the settings, played once so you hear it.
    Preview(Sound),
    /// Text dragged out of the pane, for the clipboard.
    Copy(String),
    /// A web page for the browser (a pull request, a check's log).
    OpenUrl(String),
    /// A desktop notification without a sound, when the terminal is not in front.
    Notify(String),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub text: String,
    /// It waits for you; otherwise it is done.
    pub waiting: bool,
}

pub struct App {
    pub state: StateSnapshot,
    pub project: Option<ProjectId>,
    pub selected: Option<SessionId>,
    /// The stand-in of a band with no cards, selected when no card is (its folder).
    pub empty: Option<PathBuf>,
    /// The worktree `X` asked to remove, until the daemon answers.
    removing: Option<PathBuf>,
    /// Tasks waiting for their new worktrees, by `CreateWorktree` ticket: what to start
    /// once each is made.
    task_for: HashMap<u64, QuickPrompt>,
    /// Each project's worktrees, as the daemon last sent them.
    pub worktrees: HashMap<ProjectId, Vec<termist_core::WorktreeInfo>>,
    pub mode: Mode,
    pub screens: HashMap<SessionId, Snapshot>,
    pub attached: Option<SessionId>,
    pub pane: (u16, u16),
    /// Where the last frame drew the pane's inside: the wheel scrolls only over it.
    pub pane_area: Rect,
    /// The pane shows the attached session's history: the scroll keys have the
    /// keyboard, on top of the grid or focus mode.
    pub scrolling: bool,
    /// Copy mode, over the scrolled pane: its cursor and selection.
    pub copy: Option<Copy>,
    /// The session whose text `y` asked for.
    copying: Option<SessionId>,
    /// Pane text being dragged over with the mouse, highlighted until the next press.
    pub selection: Option<Selection>,
    /// Notes in the top right corner.
    pub toasts: Toasts,
    /// The last press landed on a toast: its drag and release are not a selection.
    toast_down: bool,
    pub cards_per_row: usize,
    /// Lines the cards may take on screen; the rows of them shown from the first one.
    pub card_lines: u16,
    pub card_rows: usize,
    pub card_scroll: usize,
    /// The grid, the archive, or the pull requests.
    pub view: View,
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
    /// The text of a quick prompt closed without starting it; the next one opens on it.
    prompt_draft: Option<String>,
    /// Recently used models per harness, as the daemon last sent them.
    recent_models: HashMap<Harness, Vec<String>>,
    /// Each CLI's own model list per harness, as the daemon last sent it.
    pub model_catalogs: HashMap<Harness, Vec<ModelInfo>>,
    focus_next_created: bool,
    /// The card selected when a tool was asked for, and the tool's card with it once it
    /// came: when the tool ends, the selection goes back there.
    tool_from: Option<SessionId>,
    tool_back: Option<(SessionId, SessionId)>,
    resume_pending: Option<SessionId>,
    /// A project asked to be opened (or a folder added); switched to when it arrives.
    project_pending: Option<ProjectPending>,
    pub config: Config,
    /// Where config.toml is, for the help screen; `None` in tests.
    pub config_path: Option<PathBuf>,
    pub theme: Theme,
    /// Every theme there is: the built-in ones and the user's own.
    pub themes: Themes,
    pub keymap: Keymap,
    /// What `colors = "auto"` means in this terminal.
    pub detected_depth: ColorDepth,
    /// The host terminal's own colours, if it said; agents get them with a theme that
    /// paints nothing.
    pub host_colors: Option<TermColors>,
    /// The settings config.local.toml sets (`theme`, `notify.sounds`, and `keys` for any
    /// key): the settings screen leaves them.
    pub local_settings: Vec<String>,
    /// `C-a z`: the pane's place until termist quits, over the configured one.
    pub pane_override: Option<PanePosition>,
    /// Where the last frame put the pane.
    pub pane_beside: bool,
    /// How far the help can scroll in the last frame: its last line at the bottom.
    pub help_end: std::cell::Cell<usize>,
    /// A scene over the screen: the splash, or the idle screen.
    pub showing: Option<Showing>,
    /// The scene of the empty grid and the help, picked at start.
    pub scene: &'static str,
    /// The scenes picked so far, loaded.
    pub scenes: HashMap<&'static str, Scene>,
    /// The last key or paste: the idle screen comes `idle_minutes` after it.
    pub last_input: Instant,
    /// When termist started: the empty grid's scene moves from here.
    pub started: Instant,
    /// The hour on the local clock, for the scenes' palettes.
    pub hour: u32,
    /// The minute on the local clock, for the status line.
    pub minute: u32,
    /// The last reading for the status line.
    pub sysstat: termist_platform::sysstat::SysStat,
    /// The whole screen at the last frame: a scene moves only where it fits.
    pub screen: ratatui::layout::Rect,
    /// The terminal window is in front (focus reports; assumed without them).
    pub window_focused: bool,
    /// Each project's pull requests, as the daemon last sent them.
    pub prs: HashMap<ProjectId, ProjectPrs>,
    /// Each project's open issues, as the daemon last sent them while looked at.
    pub issues: HashMap<ProjectId, ProjectIssues>,
    /// Each project's repos and the logged-in accounts, for the repos window.
    pub repo_lists: HashMap<ProjectId, (Vec<String>, Vec<RepoInfo>)>,
    /// Pull requests read whole: how the last read went, and the last good one.
    pub pr_details: HashMap<PrRef, (GhState, Option<PrDetail>)>,
    /// Each pull request's diff as last read, and how the newest read went.
    pub pr_diffs: HashMap<PrRef, (GhState, Option<PrDiff>)>,
    /// Words written to a pull request and not sent yet, by what they go to.
    pub drafts: HashMap<(PrRef, Target), String>,
    /// Writes on their way, by ticket: the draft to forget once GitHub took them.
    writes: HashMap<u64, Option<(PrRef, Target)>>,
    /// The worktree asked for with `w` (or `a`): its pull request, and the words the
    /// quick prompt opens with there (`None`: the pull request's own line).
    worktree_for: Option<(PrRef, Option<String>)>,
    /// Review threads marked with `Space`, by pull request, to hand to an agent.
    pub marks: HashMap<PrRef, BTreeSet<String>>,
    /// Threads of this pull request are on their way to an agent: its marks go once
    /// they are sent.
    hand_for: Option<PrRef>,
    /// The comment `D` asked to delete, waiting for a yes.
    pub deleting: Option<(PrRef, String, CommentKind)>,
    next_ticket: u64,
    /// Where the last frame put the PR view's rows and threads.
    pub pr_layout: RefCell<PrLayout>,
    /// What the last frame drew where, for the mouse.
    pub hits: RefCell<crate::hit::Hits>,
    /// Tests fix the clock; ages are counted from it.
    pub frozen_now: Option<i64>,
    /// What the daemon was last told this client looks at: a project, its open pull
    /// request, that one's diff, its issues.
    pr_focus: (Option<ProjectId>, Option<PrRef>, bool, bool),
    rng: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProjectPending {
    /// A known project being opened, and the card to select once it is.
    Known {
        project: ProjectId,
        select: Option<SessionId>,
    },
    /// A folder added under its real path; the project comes with exactly that path.
    Path(PathBuf),
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Default settings in the terminal's own colours.
    pub fn new() -> App {
        let mut app = App::with_config(Config::default(), Theme::terminal(), Keymap::defaults());
        // The same scene on every run, for tests.
        app.rng = 1;
        app.scene = "";
        app.scene = app.pick_scene();
        app
    }

    pub fn with_config(config: Config, theme: Theme, keymap: Keymap) -> App {
        let mut app = App {
            state: StateSnapshot::default(),
            project: None,
            selected: None,
            empty: None,
            removing: None,
            task_for: HashMap::new(),
            worktrees: HashMap::new(),
            mode: Mode::Grid,
            screens: HashMap::new(),
            attached: None,
            pane: (0, 0),
            pane_area: Rect::default(),
            selection: None,
            toasts: Toasts::default(),
            toast_down: false,
            scrolling: false,
            copy: None,
            copying: None,
            cards_per_row: 1,
            card_lines: crate::ui::CARD_H,
            card_rows: 1,
            card_scroll: 0,
            view: View::Grid,
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
            prompt_draft: None,
            recent_models: HashMap::new(),
            model_catalogs: HashMap::new(),
            focus_next_created: false,
            tool_from: None,
            tool_back: None,
            resume_pending: None,
            project_pending: None,
            config,
            config_path: None,
            theme,
            themes: Themes::builtin(),
            keymap,
            detected_depth: ColorDepth::TrueColor,
            host_colors: None,
            local_settings: Vec::new(),
            pane_override: None,
            pane_beside: false,
            help_end: std::cell::Cell::new(usize::MAX),
            showing: None,
            scene: "",
            scenes: HashMap::new(),
            last_input: Instant::now(),
            started: Instant::now(),
            hour: 12,
            minute: 0,
            sysstat: Default::default(),
            window_focused: true,
            prs: HashMap::new(),
            issues: HashMap::new(),
            repo_lists: HashMap::new(),
            pr_details: HashMap::new(),
            pr_diffs: HashMap::new(),
            drafts: HashMap::new(),
            writes: HashMap::new(),
            worktree_for: None,
            marks: HashMap::new(),
            hand_for: None,
            deleting: None,
            next_ticket: 0,
            pr_layout: RefCell::default(),
            hits: RefCell::default(),
            frozen_now: None,
            pr_focus: (None, None, false, false),
            screen: ratatui::layout::Rect::default(),
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64 | 1),
        };
        app.scene = app.pick_scene();
        app
    }

    fn alert(&mut self, id: SessionId, waiting: bool) -> Option<Action> {
        let s = self.state.sessions.iter().find(|s| s.id == id)?;
        if s.archived {
            return None;
        }
        let project = self
            .state
            .projects
            .iter()
            .find(|p| p.id == s.project)
            .map_or("", |p| p.name.as_str());
        let what = if waiting { "waits for you" } else { "is done" };
        let text = format!("{} · {project} {what}", s.display_name());
        if self.config.notify.toasts {
            let status = s.status;
            let (glyph, _, _) = crate::ui::status_style(&self.theme, status);
            self.toasts.push(Toast {
                text: format!("{glyph} {text}"),
                kind: ToastKind::Agent {
                    session: id,
                    status,
                },
                until: Instant::now() + toast::AGENT_FOR,
            });
        }
        Some(Action::Alert(Alert { text, waiting }))
    }

    /// A scene from the configured pool, never the one shown last, loaded.
    pub fn pick_scene(&mut self) -> &'static str {
        let pool: Vec<&'static str> = termist_scenes::names()
            .filter(|n| self.config.scenes.pool.iter().any(|p| p == n))
            .collect();
        let pool = if pool.is_empty() {
            termist_scenes::names().collect()
        } else {
            pool
        };
        let fresh: Vec<&'static str> = pool
            .iter()
            .copied()
            .filter(|n| pool.len() == 1 || *n != self.scene)
            .collect();
        // xorshift: enough to vary the scene from one start to the next.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let name = fresh[(self.rng % fresh.len() as u64) as usize];
        if !self.scenes.contains_key(name)
            && let Ok(scene) = Scene::load(name)
        {
            self.scenes.insert(name, scene);
        }
        name
    }

    pub fn time_of_day(&self) -> TimeOfDay {
        TimeOfDay::from_hour(self.hour)
    }

    /// The splash, if the settings want it and the terminal can draw it.
    pub fn start_splash(&mut self, now: Instant) {
        if self.config.scenes.splash && self.theme.draws_scenes() {
            self.showing = Some(Showing {
                name: self.scene,
                since: now,
                kind: ShowKind::Splash,
            });
        }
    }

    /// The frame of the scene on screen: counted from when it came up.
    pub fn scene_frame(&self, now: Instant) -> u64 {
        let since = self.showing.map_or(self.started, |s| s.since);
        scene_view::frame_number(now.saturating_duration_since(since), self.config.animations)
    }

    /// Ends the splash after its second and brings up the idle screen when it is due.
    pub fn tick(&mut self, now: Instant) {
        self.toasts.expire(now);
        match self.showing {
            Some(Showing {
                kind: ShowKind::Splash,
                since,
                ..
            }) if now.saturating_duration_since(since) >= SPLASH => self.showing = None,
            None if self.idle_due(now) => {
                let name = self.pick_scene();
                self.scene = name;
                self.showing = Some(Showing {
                    name,
                    since: now,
                    kind: ShowKind::Idle,
                });
            }
            _ => {}
        }
    }

    fn idle_due(&self, now: Instant) -> bool {
        let minutes = self.config.scenes.idle_minutes;
        minutes > 0
            && self.theme.draws_scenes()
            && now.saturating_duration_since(self.last_input)
                >= Duration::from_secs(minutes as u64 * 60)
    }

    /// When the screen must be drawn again with no input: a scene's next frame, the end
    /// of the splash, the idle scene, a toast going away.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let finder = self.overlays.iter().find_map(|o| match o {
            Overlay::Finder(f) => f.due,
            _ => None,
        });
        [self.scene_wake(now), self.toasts.next_expiry(), finder]
            .into_iter()
            .flatten()
            .min()
    }

    fn scene_wake(&self, now: Instant) -> Option<Instant> {
        let frame = now + Duration::from_millis(1000 / scene_view::FPS);
        // A scene that does not fit is the wordmark, which does not move.
        let fits = |name: &str, spare_rows: u16, spare_cols: u16| {
            self.scenes.get(name).is_some_and(|scene| {
                self.screen.width as usize >= scene.width + spare_cols as usize
                    && self.screen.height as usize >= scene.height + spare_rows as usize
            })
        };
        let scene_up = match self.showing {
            Some(Showing {
                name,
                kind: ShowKind::Splash,
                ..
            }) => fits(name, 3, 0),
            Some(Showing { name, .. }) => fits(name, 2 + 2, 0),
            None if matches!(self.overlays.last(), Some(Overlay::Help { .. })) => {
                fits(self.scene, 2, 2)
            }
            None => {
                self.connected
                    && self.project_sessions().is_empty()
                    && self.view == View::Grid
                    && self.overlays.is_empty()
                    && fits(self.scene, 2 + 3, 0)
            }
        };
        if scene_up && self.config.animations && self.theme.draws_scenes() {
            return Some(frame);
        }
        match self.showing {
            Some(Showing {
                kind: ShowKind::Splash,
                since,
                ..
            }) => Some(since + SPLASH),
            Some(_) => None,
            None if self.config.scenes.idle_minutes > 0 && self.theme.draws_scenes() => Some(
                self.last_input + Duration::from_secs(self.config.scenes.idle_minutes as u64 * 60),
            ),
            None => None,
        }
    }

    /// The cards of the current project: its archived ones in the archive view, the
    /// others everywhere else.
    pub fn project_sessions(&self) -> Vec<&SessionInfo> {
        self.state
            .sessions
            .iter()
            .filter(|s| Some(s.project) == self.project && s.archived == self.archive_view())
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

    /// Agents waiting on the user in closed projects: counted in the tab bar and
    /// reachable with `.` / `,`.
    pub fn waiting_in_closed_projects(&self) -> Vec<SessionInfo> {
        self.state
            .sessions
            .iter()
            .filter(|s| {
                !s.archived
                    && s.status == AgentStatus::NeedsFeedback
                    && self
                        .state
                        .projects
                        .iter()
                        .any(|p| p.id == s.project && !p.open)
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
                let before = self
                    .state
                    .sessions
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.status);
                // An agent that starts waiting ends the idle screen, on its card.
                let starts_waiting = status == AgentStatus::NeedsFeedback
                    && before.is_some_and(|b| b != AgentStatus::NeedsFeedback)
                    && !info.archived;
                let idle = matches!(self.showing, Some(s) if s.kind == ShowKind::Idle);
                // Archived elsewhere while focused here: back to the grid.
                if info.archived
                    && !self.archive_view()
                    && self.selected == Some(id)
                    && matches!(
                        self.mode,
                        Mode::Focus | Mode::FocusPrefix | Mode::ConfirmArchive(_)
                    )
                {
                    self.mode = Mode::Grid;
                }
                // You are looking at it: a focused session that finishes is seen.
                // A scene over the screen hides the pane: nobody is looking.
                let seen_now = info.status == AgentStatus::Unseen
                    && self.showing.is_none()
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
                    // A tool goes back to where it was opened from when it ends.
                    if matches!(
                        self.state
                            .sessions
                            .iter()
                            .find(|s| s.id == id)
                            .map(|s| &s.kind),
                        Some(SessionKind::Tool { .. })
                    ) {
                        self.tool_back = self.tool_from.take().map(|from| (id, from));
                    }
                    self.arrived(id);
                }
                self.repair_selection();
                if seen_now {
                    actions.push(Action::Send(ClientRequest::MarkSeen { session: id }));
                }
                // A sound and a note for a card that starts waiting or finishes, unless
                // you are looking at it.
                let watching = self.window_focused
                    && self.showing.is_none()
                    && self.selected == Some(id)
                    && matches!(self.mode, Mode::Focus | Mode::FocusPrefix);
                let finishes = status == AgentStatus::Unseen
                    && before.is_some_and(|b| b != AgentStatus::Unseen);
                if (starts_waiting || finishes) && !watching {
                    actions.extend(self.alert(id, starts_waiting));
                }
                if starts_waiting && idle {
                    self.showing = None;
                    self.last_input = Instant::now();
                    let shown = self.view == View::Grid
                        && self.visible_sessions().iter().any(|s| s.id == id);
                    if matches!(self.mode, Mode::Grid) && shown {
                        self.select(id);
                        self.repair_selection();
                    }
                }
                if self.resume_pending == Some(id) && status == AgentStatus::Fresh {
                    self.resume_pending = None;
                    self.attached = None; // the old process's attachment ended with it
                    self.arrived(id);
                }
            }
            ServerEvent::SessionRemoved(id) => {
                let back = self
                    .tool_back
                    .filter(|(tool, _)| *tool == id)
                    .map(|(_, from)| from);
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
                if let Some(from) =
                    back.filter(|from| self.state.sessions.iter().any(|s| s.id == *from))
                {
                    self.tool_back = None;
                    self.select(from);
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
            ServerEvent::Models {
                harness,
                recent,
                catalog,
            } => {
                for o in &mut self.overlays {
                    if let Overlay::Model(m) = o
                        && m.harness == harness
                    {
                        let current = m.current.clone();
                        m.set_lists(recent.clone(), &catalog, current.as_deref());
                    }
                }
                self.recent_models.insert(harness, recent);
                self.model_catalogs.insert(harness, catalog);
            }
            ServerEvent::Prs {
                project,
                state,
                discovered,
                repos,
            } => {
                self.prs.insert(
                    project,
                    ProjectPrs {
                        state,
                        discovered,
                        repos,
                    },
                );
            }
            ServerEvent::Repos {
                project,
                accounts,
                repos,
            } => {
                for o in &mut self.overlays {
                    if let Overlay::Repos { project: p, picker } = o
                        && *p == project
                    {
                        picker.set_items(repos.clone(), overlay::repo_label);
                    }
                }
                self.repo_lists.insert(project, (accounts, repos));
            }
            ServerEvent::Issues {
                project,
                state,
                repos,
            } => {
                self.issues.insert(project, ProjectIssues { state, repos });
            }
            ServerEvent::PrDetail { pr, state, detail } => {
                self.pr_details.insert(pr, (state, detail.map(|d| *d)));
            }
            ServerEvent::PrDiff { pr, state, diff } => {
                // A failed read brings no diff: the one shown stays, with the failure.
                let diff = diff
                    .map(|d| *d)
                    .or_else(|| self.pr_diffs.remove(&pr).and_then(|(_, d)| d));
                if let (View::Prs(view), Some(diff)) = (&mut self.view, &diff)
                    && let Some(d) = view.detail.as_mut().filter(|d| d.pr == pr)
                    && let Some(open) = &mut d.diff
                {
                    open.settle(&diff.files);
                }
                // Only the open diff is kept (each may be megabytes); the daemon sends a
                // diff again to whoever comes back to it.
                let open = match &self.view {
                    View::Prs(v) => v.detail.as_ref().filter(|d| d.diff.is_some()).map(|d| d.pr),
                    _ => None,
                };
                self.pr_diffs.retain(|p, _| Some(*p) == open);
                if open == Some(pr) {
                    self.pr_diffs.insert(pr, (state, diff));
                }
            }
            ServerEvent::Worktrees { project, list } => {
                self.worktrees.insert(project, list);
                self.repair_selection();
                if matches!(self.overlays.last(), Some(Overlay::Worktrees(_))) {
                    self.open_worktrees();
                }
            }
            ServerEvent::WorktreeMade {
                ticket,
                path,
                branch,
                note,
            } => {
                if let Some(mut q) = self.task_for.remove(&ticket) {
                    self.message = note;
                    q.new_worktree = None;
                    q.worktree = Some((path, branch));
                    return self.start_task(q);
                }
            }
            ServerEvent::WorktreeNotMade { ticket, message } => {
                if let Some(q) = self.task_for.remove(&ticket) {
                    // The words come back, with why.
                    self.message = Some(message);
                    self.overlays.push(Overlay::QuickPrompt(q));
                }
            }
            ServerEvent::RemoveRefused { path, files } => {
                if self.removing.as_ref() == Some(&path) {
                    self.mode = Mode::ConfirmRemove { files };
                }
            }
            ServerEvent::WorktreeRemoved { path } => {
                if self.removing.as_ref() == Some(&path) {
                    self.removing = None;
                    self.message = Some(format!("removed {} · the branch stays", short(&path)));
                }
            }
            ServerEvent::LocalDiff {
                path,
                mode,
                state,
                diff,
            } => {
                if let View::Diff(l) = &mut self.view
                    && l.path == path
                    && l.mode == mode
                {
                    // Not a repo: nothing to show; the grid stays, a toast says why.
                    let no_repo =
                        matches!(&state, ReadState::Failed(why) if why == mirror::NO_REPO);
                    if no_repo && diff.is_none() && l.diff.is_none() {
                        actions.extend(self.close_local_diff());
                        self.toasts.push(Toast {
                            text: format!("✗ {}", mirror::NO_REPO),
                            kind: ToastKind::Failed,
                            until: Instant::now() + toast::AGENT_FOR,
                        });
                    } else {
                        l.arrived(state, diff.map(|d| *d));
                    }
                }
            }
            ServerEvent::EditorFailed { message } => {
                self.focus_next_created = false;
                self.tool_from = None;
                self.toasts.push(Toast {
                    text: format!("✗ {message}"),
                    kind: ToastKind::Failed,
                    until: Instant::now() + toast::AGENT_FOR,
                });
            }
            ServerEvent::Files {
                ticket,
                root,
                files,
                more,
            } => {
                if let Some(f) = self.finder_for(ticket) {
                    f.set_files(root, files, more);
                }
            }
            ServerEvent::GrepResults {
                ticket,
                root,
                matches,
                more,
            } => {
                if let Some(f) = self.finder_for(ticket) {
                    f.set_grep(root, matches, more);
                }
            }
            ServerEvent::FindFailed { ticket, message } => {
                // Not a repo: nothing to find there; the box goes, a toast says why.
                let gone = self.finder_for(ticket).map(|f| {
                    f.waiting = false;
                    let nothing = f.root.is_none() && message == crate::diff::local::NO_REPO;
                    f.failed = Some(message.clone());
                    nothing
                });
                if gone == Some(true) {
                    self.overlays
                        .retain(|o| !matches!(o, Overlay::Finder(f) if f.ticket == ticket));
                    self.toasts.push(Toast {
                        text: format!("✗ {message}"),
                        kind: ToastKind::Failed,
                        until: Instant::now() + toast::AGENT_FOR,
                    });
                }
            }
            ServerEvent::CopiedText { session, text } => {
                if self.copying == Some(session) {
                    self.copying = None;
                    let n = text.lines().count().max(1);
                    let what = if n == 1 { "line" } else { "lines" };
                    self.toasts.push(Toast {
                        text: format!("✓ copied {n} {what}"),
                        kind: ToastKind::Copied,
                        until: Instant::now() + toast::COPIED_FOR,
                    });
                    actions.push(Action::Copy(text));
                }
            }
            ServerEvent::Found {
                session,
                at,
                index,
                total,
            } => {
                if let Some(copy) = self.copy.as_mut().filter(|c| c.session == session)
                    && let Some(screen) = self.screens.get(&session)
                {
                    let outs = copy.found_at(at, index, total, screen);
                    actions.extend(self.copy_outs(outs));
                }
            }
            ServerEvent::RemoveFailed { path, message } => {
                if self.removing.as_ref() == Some(&path) {
                    self.removing = None;
                    self.message = Some(message);
                }
            }
            ServerEvent::WorktreeReady { pr, path, .. } => {
                // Only the one asked for; another client's answer leaves it waiting.
                if self
                    .worktree_for
                    .as_ref()
                    .is_some_and(|(want, _)| *want == pr)
                    && let Some((_, text)) = self.worktree_for.take()
                {
                    return self.worktree_ready(pr, path, text);
                }
            }
            ServerEvent::WorktreeFailed { pr, message } => {
                if self
                    .worktree_for
                    .as_ref()
                    .is_some_and(|(want, _)| *want == pr)
                {
                    self.worktree_for = None;
                    self.hand_for = None; // nothing was handed over
                    match self.view {
                        View::Prs(_) => self.message = Some(message),
                        _ => self.toasts.push(Toast {
                            text: format!("✗ {message}"),
                            kind: ToastKind::Failed,
                            until: Instant::now() + toast::AGENT_FOR,
                        }),
                    }
                }
            }
            ServerEvent::PrWritten { ticket, .. } => {
                if let Some(Some(target)) = self.writes.remove(&ticket) {
                    self.drafts.remove(&target);
                }
                if matches!(self.overlays.last(), Some(Overlay::Compose(c)) if c.state == Sending::Sending(ticket))
                {
                    self.overlays.pop();
                }
            }
            ServerEvent::PrWriteFailed {
                ticket: Some(ticket),
                message,
                ..
            } => {
                self.writes.remove(&ticket);
                match self.overlays.last_mut() {
                    // The box still open says why, and keeps the words.
                    Some(Overlay::Compose(c)) if c.state == Sending::Sending(ticket) => {
                        let why = message.split(" · ").last().unwrap_or(&message).to_string();
                        c.state = Sending::Failed(why);
                    }
                    _ => self.toasts.push(Toast {
                        text: format!("✗ {message}"),
                        kind: ToastKind::Failed,
                        until: Instant::now() + toast::AGENT_FOR,
                    }),
                }
            }
            ServerEvent::PrWriteFailed { pr, message, .. } => {
                if let View::Prs(view) = &mut self.view
                    && let Some(d) = view.detail.as_mut().filter(|d| d.pr == pr)
                    && let Some(open) = &mut d.diff
                {
                    open.pending.clear();
                }
                self.toasts.push(Toast {
                    text: format!("✗ {message}"),
                    kind: ToastKind::Failed,
                    until: Instant::now() + toast::AGENT_FOR,
                });
            }
            ServerEvent::Notice { text } => {
                if self.config.notify.toasts {
                    self.toasts.push(Toast {
                        text: format!("↳ {text}"),
                        kind: ToastKind::Notice,
                        until: Instant::now() + toast::AGENT_FOR,
                    });
                }
            }
            // `termist spawn` and `termist worktree` wait for these; the TUI asks neither.
            ServerEvent::Spawned { .. }
            | ServerEvent::SpawnFailed { .. }
            | ServerEvent::Moved { .. }
            | ServerEvent::MoveFailed { .. } => {}
            ServerEvent::ReviewRequested {
                project,
                pr,
                repo,
                title,
            } => {
                if self.config.github.enabled {
                    if self.config.notify.toasts {
                        self.toasts.push(Toast {
                            text: format!("⇄ #{} · {repo} wants your review", pr.number),
                            kind: ToastKind::Review { project, pr },
                            until: Instant::now() + toast::AGENT_FOR,
                        });
                    }
                    actions.push(Action::Notify(format!(
                        "{repo} #{} wants your review: {title}",
                        pr.number
                    )));
                }
            }
            ServerEvent::Hello { .. } | ServerEvent::Ack => {}
        }
        actions.extend(self.sync_attachment());
        actions.extend(self.sync_prs());
        actions
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        self.last_input = Instant::now();
        self.selection = None;
        // A key on a scene only takes it away.
        if self.showing.take().is_some() {
            return vec![];
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('q') {
            // Ctrl+Q always gets you out: every overlay closes and focus mode ends.
            for o in std::mem::take(&mut self.overlays) {
                if let Overlay::QuickPrompt(q) = o {
                    self.keep_draft(&q);
                }
            }
            self.mode = Mode::Grid;
            return self.stop_scrolling();
        }
        let mut actions = Vec::new();
        if !self.overlays.is_empty() {
            actions.extend(self.overlay_key(key));
            actions.extend(self.sync_attachment());
            actions.extend(self.sync_prs());
            return actions;
        }
        if self.scrolling && matches!(self.mode, Mode::Grid | Mode::Focus) {
            match self.copy.is_some() {
                true => actions.extend(self.copy_key(key)),
                false => actions.extend(self.scroll_key(key)),
            }
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
            Mode::ConfirmRemove { files } => {
                self.mode = Mode::Grid;
                if let Some(path) = self.removing.clone()
                    && matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
                {
                    return vec![Action::Send(ClientRequest::RemoveWorktree {
                        path,
                        force: files > 0,
                    })];
                }
                self.removing = None;
                return vec![];
            }
            Mode::ConfirmDelete => {
                self.mode = Mode::Grid;
                let deleting = self.deleting.take();
                if let Some((pr, comment, kind)) = deleting
                    && matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
                {
                    return self.write_now(pr, PrWrite::Delete { comment, kind });
                }
                return vec![];
            }
            Mode::Grid if matches!(self.view, View::Diff(_)) => {
                self.message = None;
                actions.extend(self.local_key(key));
            }
            Mode::Grid if matches!(self.view, View::Prs(_)) => {
                self.message = None;
                actions.extend(self.prs_key(key));
            }
            Mode::Grid if self.archive_view() => {
                self.message = None;
                actions.extend(self.archive_key(key));
            }
            Mode::Focus => {
                if self.keymap.prefix.matches(&key) {
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
                if self.keymap.prefix.matches(&key) {
                    // The prefix twice sends it to the session.
                    if let Some(id) = self.selected {
                        let modes = self.screens.get(&id).map(|s| s.modes).unwrap_or_default();
                        let data = encode_key(&key, &modes);
                        actions.push(Action::Send(ClientRequest::Input { session: id, data }));
                    }
                } else if let Some(action) = self.keymap.action(Context::Focus, &key) {
                    actions.extend(self.act(action));
                }
            }
            Mode::Grid => {
                // An error message stays up only until the next key.
                self.message = None;
                if ctrl && key.code == KeyCode::Char('c') {
                    self.mode = Mode::ConfirmQuit;
                } else if let Some(action) = self.keymap.action(Context::Grid, &key) {
                    actions.extend(self.act(action));
                }
            }
        }
        actions.extend(self.sync_attachment());
        actions.extend(self.sync_prs());
        actions
    }

    pub fn on_paste(&mut self, text: &str) -> Vec<Action> {
        self.last_input = Instant::now();
        if self.showing.take().is_some() {
            return vec![];
        }
        if let Some(top) = self.overlays.last_mut() {
            if let Some(input) = top.text_input_mut() {
                input.insert_str(text);
            }
            return vec![];
        }
        match (self.mode, self.selected) {
            (Mode::Focus, Some(id)) => {
                // The daemon shows the live screen again for any input.
                self.scrolling = false;
                self.copy = None;
                let modes = self.screens.get(&id).map(|s| s.modes).unwrap_or_default();
                vec![Action::Send(ClientRequest::Input {
                    session: id,
                    data: encode_paste(text, &modes),
                })]
            }
            _ => vec![],
        }
    }

    /// The wheel over the pane. A program that asked for the mouse gets the notch; a
    /// full-screen one that did not gets arrow keys (it keeps no history); otherwise the
    /// view moves through the session's history.
    pub fn on_mouse(&mut self, ev: MouseEvent) -> Vec<Action> {
        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self.overlays.is_empty()
            && self.showing.is_none()
            && matches!(self.mode, Mode::Grid | Mode::Focus | Mode::FocusPrefix)
            && self.toasts.hit(self.screen, ev.column, ev.row).is_none()
        {
            let tab = self.hits.borrow().tab_at(ev.column, ev.row);
            if let Some(project) = tab {
                return self.click_tab(project);
            }
        }
        if matches!(self.view, View::Diff(_))
            && self.overlays.is_empty()
            && self.showing.is_none()
            && self.toasts.hit(self.screen, ev.column, ev.row).is_none()
        {
            return self.local_mouse(ev);
        }
        if matches!(self.view, View::Prs(_)) && self.overlays.is_empty() && self.showing.is_none() {
            let on_toast = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
                && self.toasts.hit(self.screen, ev.column, ev.row).is_some();
            if !on_toast {
                let mut actions = self.prs_mouse(ev);
                actions.extend(self.sync_prs());
                return actions;
            }
        }
        let up = match ev.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                if self.showing.is_none()
                    && let Some(i) = self.toasts.hit(self.screen, ev.column, ev.row)
                {
                    self.toast_down = true;
                    // Under an overlay a click only dismisses the toast.
                    let kind = self.toasts.remove(i).map(|t| t.kind);
                    if !self.overlays.is_empty() {
                        return vec![];
                    }
                    return match kind {
                        Some(ToastKind::Agent { session, .. }) => {
                            // It lands in the grid: the next key goes to no other card.
                            let mut actions = self.stop_scrolling();
                            self.mode = Mode::Grid;
                            actions.extend(self.reveal(session));
                            actions.extend(self.sync_attachment());
                            actions.extend(self.sync_prs());
                            actions
                        }
                        Some(ToastKind::Review { project, pr }) => self.reveal_pr(project, pr),
                        _ => vec![],
                    };
                }
                self.toast_down = false;
                if self.overlays.is_empty()
                    && matches!(self.mode, Mode::Grid | Mode::Focus | Mode::FocusPrefix)
                {
                    let (card, more, band_pr, empty) = {
                        let hits = self.hits.borrow();
                        (
                            hits.card_at(ev.column, ev.row),
                            hits.more_at(ev.column, ev.row),
                            hits.band_pr_at(ev.column, ev.row),
                            hits.empty_at(ev.column, ev.row),
                        )
                    };
                    if let Some(path) = empty {
                        // A second click on a band's stand-in starts a task there.
                        let again = self.selected.is_none() && self.empty.as_ref() == Some(&path);
                        self.mode = Mode::Grid;
                        self.set_slot(Slot::Empty(path));
                        if again {
                            return self.start_in_empty();
                        }
                        return self.sync_attachment();
                    }
                    if let (Some(pr), Some(project)) = (band_pr, self.project) {
                        return self.reveal_pr(project, pr);
                    }
                    if let Some(id) = card {
                        return self.click_card(id);
                    }
                    if let Some(rows) = more {
                        self.mode = Mode::Grid;
                        self.move_rows(rows);
                        return self.sync_attachment();
                    }
                }
                if self.takes_mouse(ev) {
                    self.selection = self.attached.map(|id| Selection::new(id, self.in_pane(ev)));
                }
                return vec![];
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.toast_down {
                    return vec![];
                }
                let at = self.in_pane(ev);
                if let Some(selection) = &mut self.selection {
                    selection.head = at;
                }
                return vec![];
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if std::mem::take(&mut self.toast_down) {
                    return vec![];
                }
                // A click on the pane, not a drag: into the session, as Enter.
                let click = self.selection.as_ref().is_some_and(|s| s.anchor == s.head);
                let actions = self.copy_selection();
                if click && self.mode == Mode::Grid && self.view == View::Grid {
                    return self.enter();
                }
                return actions;
            }
            _ => return vec![],
        };
        if !self.takes_mouse(ev) {
            let cards = self.hits.borrow().over_cards(ev.column, ev.row);
            if cards && self.overlays.is_empty() && self.mode == Mode::Grid {
                self.move_rows(if up { -1 } else { 1 });
                return self.sync_attachment();
            }
            return vec![];
        }
        let Some(id) = self.attached else {
            return vec![];
        };
        self.last_input = Instant::now();
        self.selection = None;
        let modes = self.screens.get(&id).map(|s| s.modes).unwrap_or_default();
        if modes.mouse_reporting {
            let (col, row) = (ev.column - self.pane_area.x, ev.row - self.pane_area.y);
            let data = encode_wheel(up, col, row, &modes);
            return vec![Action::Send(ClientRequest::Input { session: id, data })];
        }
        if modes.alt_screen {
            let arrow = KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down });
            let data = encode_key(&arrow, &modes).repeat(WHEEL_LINES as usize);
            return vec![Action::Send(ClientRequest::Input { session: id, data })];
        }
        match (up, self.scrolling) {
            (true, false) => self.start_scrolling(WHEEL_LINES),
            (true, true) => self.scroll_by(Scroll::Lines(WHEEL_LINES as i32)),
            (false, true) => self.scroll_down(WHEEL_LINES),
            (false, false) => vec![],
        }
    }

    /// A card was clicked: it is selected, out of focus mode; a second click on the
    /// selected card goes into it, as Enter.
    fn click_card(&mut self, id: SessionId) -> Vec<Action> {
        let focused = matches!(self.mode, Mode::Focus | Mode::FocusPrefix);
        let again = !focused && self.selected == Some(id) && self.view == View::Grid;
        let mut actions = self.stop_scrolling();
        self.mode = Mode::Grid;
        self.select(id);
        if again {
            actions.extend(self.enter());
        }
        actions.extend(self.sync_attachment());
        actions
    }

    /// A project tab was clicked: that project, as its number key would; out of focus
    /// mode first.
    fn click_tab(&mut self, project: ProjectId) -> Vec<Action> {
        let mut actions = self.stop_scrolling();
        self.mode = Mode::Grid;
        self.go_to_project(project);
        actions.extend(self.sync_attachment());
        actions.extend(self.sync_prs());
        actions
    }

    /// The mouse is over the pane and nothing sits on top of it.
    fn takes_mouse(&self, ev: MouseEvent) -> bool {
        self.showing.is_none()
            && self.overlays.is_empty()
            && matches!(self.mode, Mode::Grid | Mode::Focus)
            && self.pane_area.contains(Position::new(ev.column, ev.row))
    }

    /// Where the mouse is in the pane's cells, held to its edges.
    fn in_pane(&self, ev: MouseEvent) -> (u16, u16) {
        let a = self.pane_area;
        let col = ev.column.clamp(a.x, a.right().saturating_sub(1)) - a.x;
        let row = ev.row.clamp(a.y, a.bottom().saturating_sub(1)) - a.y;
        (col, row)
    }

    /// The button came up: a drag puts its text on the clipboard and stays
    /// highlighted; a click without one leaves nothing selected.
    fn copy_selection(&mut self) -> Vec<Action> {
        let Some(selection) = &self.selection else {
            return vec![];
        };
        let text = match self.screens.get(&selection.session) {
            Some(screen) if selection.anchor != selection.head => selection.text(screen),
            _ => String::new(),
        };
        if text.is_empty() {
            self.selection = None;
            return vec![];
        }
        let n = text.chars().count();
        let what = if n == 1 { "character" } else { "characters" };
        self.toasts.push(Toast {
            text: format!("✓ copied {n} {what}"),
            kind: ToastKind::Copied,
            until: Instant::now() + toast::COPIED_FOR,
        });
        vec![Action::Copy(text)]
    }

    /// Starts scrolling the pane `lines` back, if its session has history to show. A
    /// full-screen program (Claude Code, OpenCode) keeps its own history: it gets a
    /// PageUp and scrolls itself.
    fn start_scrolling(&mut self, lines: u32) -> Vec<Action> {
        let Some((id, screen)) = self
            .attached
            .and_then(|id| self.screens.get(&id).map(|s| (id, s)))
        else {
            return vec![];
        };
        if screen.modes.alt_screen {
            let data = encode_key(&KeyEvent::from(KeyCode::PageUp), &screen.modes);
            return vec![Action::Send(ClientRequest::Input { session: id, data })];
        }
        if screen.scroll.history == 0 {
            self.message = Some("nothing to scroll back to yet".into());
            return vec![];
        }
        self.scrolling = true;
        self.scroll_by(Scroll::Lines(lines as i32))
    }

    fn scroll_by(&self, scroll: Scroll) -> Vec<Action> {
        match self.attached {
            Some(session) => vec![Action::Send(ClientRequest::Scroll { session, scroll })],
            None => vec![],
        }
    }

    /// Forward `lines`; reaching the live screen stops scrolling.
    fn scroll_down(&mut self, lines: u32) -> Vec<Action> {
        let offset = self
            .attached
            .and_then(|id| self.screens.get(&id))
            .map_or(0, |s| s.scroll.offset);
        if lines >= offset {
            return self.stop_scrolling();
        }
        self.scroll_by(Scroll::Lines(-(lines as i32)))
    }

    /// Copy mode (`C-a [`, `PgUp`): a cursor on the session's screen, `lines` up from
    /// its last line; the history can be copied from even before there is any.
    fn start_copy(&mut self, lines: u32) -> Vec<Action> {
        let Some((id, screen)) = self
            .attached
            .and_then(|id| self.screens.get(&id).map(|s| (id, s)))
        else {
            return vec![];
        };
        if screen.modes.alt_screen {
            let data = encode_key(&KeyEvent::from(KeyCode::PageUp), &screen.modes);
            return vec![Action::Send(ClientRequest::Input { session: id, data })];
        }
        let mut copy = Copy::new(id, screen);
        let outs = copy.page(-(lines as i64), screen);
        self.scrolling = true;
        self.copy = Some(copy);
        self.copy_outs(outs)
    }

    /// A key in copy mode.
    fn copy_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(copy) = self.copy.as_mut() else {
            return vec![];
        };
        let Some(screen) = self.screens.get(&copy.session) else {
            return vec![];
        };
        let outs = copy.key(key, screen);
        self.copy_outs(outs)
    }

    /// What copy mode asked: the view moved, a search or the text from the daemon,
    /// or out.
    fn copy_outs(&mut self, outs: Vec<CopyOut>) -> Vec<Action> {
        let Some(session) = self.copy.as_ref().map(|c| c.session) else {
            return vec![];
        };
        let mut actions = vec![];
        for out in outs {
            match out {
                CopyOut::Scroll(scrolls) => actions.extend(
                    scrolls
                        .into_iter()
                        .map(|scroll| Action::Send(ClientRequest::Scroll { session, scroll })),
                ),
                CopyOut::Search {
                    query,
                    from,
                    backward,
                } => actions.push(Action::Send(ClientRequest::Search {
                    session,
                    query,
                    from,
                    backward,
                })),
                CopyOut::Yank { from, to, lines } => {
                    self.copying = Some(session);
                    actions.push(Action::Send(ClientRequest::CopyText {
                        session,
                        from,
                        to,
                        lines,
                    }));
                }
                CopyOut::Exit => actions.extend(self.stop_scrolling()),
            }
        }
        actions
    }

    /// Back to the live screen, and the keys back to the grid or the session.
    fn stop_scrolling(&mut self) -> Vec<Action> {
        self.copy = None;
        if !std::mem::take(&mut self.scrolling) {
            return vec![];
        }
        self.scroll_by(Scroll::Bottom)
    }

    /// Keys while the pane shows history. None of them reaches the session.
    fn scroll_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.pane.1.max(1) as u32;
        let half = (page / 2).max(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => self.scroll_by(Scroll::Lines(1)),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_by(Scroll::Lines(page as i32)),
            KeyCode::Char('b') if ctrl => self.scroll_by(Scroll::Lines(page as i32)),
            KeyCode::PageDown => self.scroll_down(page),
            KeyCode::Char('f') if ctrl => self.scroll_down(page),
            KeyCode::Char('u') if ctrl => self.scroll_by(Scroll::Lines(half as i32)),
            KeyCode::Char('d') if ctrl => self.scroll_down(half),
            KeyCode::Char('g') | KeyCode::Home => self.scroll_by(Scroll::Top),
            KeyCode::Char('G') | KeyCode::End | KeyCode::Char('q') | KeyCode::Esc => {
                self.stop_scrolling()
            }
            KeyCode::Char('c') if ctrl => self.stop_scrolling(),
            _ => vec![],
        }
    }

    pub fn pane_resized(&mut self, cols: u16, rows: u16) -> Vec<Action> {
        if (cols, rows) == self.pane {
            return vec![];
        }
        self.pane = (cols, rows);
        self.selection = None;
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
        actions.extend(self.sync_prs());
        actions
    }

    fn create(&mut self, kind: SessionKind) -> Vec<Action> {
        let Some(project) = self.project else {
            self.message = Some("no project open".into());
            return vec![];
        };
        self.focus_next_created = true;
        let (cols, rows) = self.pane;
        // In the worktree of the selection: a band's stand-in, else the card's.
        let cwd = self
            .empty_worktree()
            .or_else(|| self.card_worktree())
            .map(|(path, _)| path);
        vec![Action::Send(ClientRequest::CreateSession {
            project,
            kind,
            cwd,
            issue: None,
            title_from: None,
            prompt: None,
            model: None,
            effort: None,
            cols: cols.max(20),
            rows: rows.max(5),
        })]
    }

    /// Sends a write that needs no words (resolving, deleting) under a new ticket.
    fn write_now(&mut self, pr: PrRef, write: PrWrite) -> Vec<Action> {
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        self.writes.insert(ticket, None);
        vec![Action::Send(ClientRequest::WritePr { pr, ticket, write })]
    }

    /// A write a key asked for, made whole with what the pull request's detail says:
    /// who started a thread, what GitHub allows, your comment, your pending review.
    fn ask(&mut self, ask: Ask) -> Vec<Action> {
        let View::Prs(view) = &self.view else {
            return vec![];
        };
        let Some(pr) = view.detail.as_ref().map(|d| d.pr) else {
            return vec![];
        };
        let Some(detail) = self.pr_details.get(&pr).and_then(|(_, d)| d.as_ref()) else {
            self.message = Some("the pull request is not read yet".into());
            return vec![];
        };
        let thread = |id: &str| detail.threads.iter().find(|t| t.id == id).cloned();
        // Your comment in question: the one named, or your last one in the thread.
        let mine = |s: &Subject| match s {
            Subject::Comment(m) => Some(m.clone()),
            Subject::Thread(id) => {
                thread(id)?
                    .comments
                    .iter()
                    .rev()
                    .find(|c| c.mine)
                    .map(|c| Mine {
                        id: c.id.clone(),
                        kind: CommentKind::Review,
                        body: c.body.clone(),
                        can_edit: c.can_edit,
                        can_delete: c.can_delete,
                    })
            }
        };
        let pending = detail
            .threads
            .iter()
            .flat_map(|t| &t.comments)
            .filter(|c| c.pending && c.mine)
            .count();
        let own = detail.mine;
        let refuse = |app: &mut App, why: &str| {
            app.message = Some(why.into());
            vec![]
        };
        match ask {
            Ask::Comment => self.open_compose(Compose::new(pr, Target::Comment, "")),
            Ask::Submit => {
                let mut c = Compose::new(pr, Target::Submit, "");
                c.mine = own;
                c.pending = pending;
                self.open_compose(c);
            }
            Ask::Line { path, target } => {
                let mut c = Compose::new(
                    pr,
                    Target::Line {
                        path,
                        side: target.side,
                        line: target.line,
                        start: target.start,
                    },
                    "",
                );
                c.context = target.context;
                c.suggest = target.new;
                self.open_compose(c);
            }
            Ask::Reply { thread: id } => {
                let Some(th) = thread(&id) else {
                    return refuse(self, "that thread is gone");
                };
                if !th.can_reply {
                    return refuse(self, "GitHub does not let you reply here");
                }
                let to = th
                    .comments
                    .first()
                    .map_or("the thread".to_string(), |c| c.author.clone());
                self.open_compose(Compose::new(pr, Target::Reply { thread: id, to }, ""));
            }
            Ask::Resolve { thread: id } => {
                let Some(th) = thread(&id) else {
                    return refuse(self, "that thread is gone");
                };
                if !th.can_resolve {
                    return refuse(self, "GitHub does not let you resolve this (yet)");
                }
                return self.write_now(
                    pr,
                    PrWrite::Resolve {
                        thread: id,
                        resolved: !th.resolved,
                    },
                );
            }
            Ask::Edit(subject) => {
                let Some(m) = mine(&subject) else {
                    return refuse(self, "not your comment");
                };
                if !m.can_edit {
                    return refuse(self, "GitHub does not let you edit this");
                }
                self.open_compose(Compose::new(
                    pr,
                    Target::Edit {
                        comment: m.id,
                        kind: m.kind,
                    },
                    &m.body,
                ));
            }
            Ask::Delete(subject) => {
                let Some(m) = mine(&subject) else {
                    return refuse(self, "not your comment");
                };
                if !m.can_delete {
                    return refuse(self, "GitHub does not let you delete this");
                }
                self.deleting = Some((pr, m.id, m.kind));
                self.mode = Mode::ConfirmDelete;
            }
        }
        vec![]
    }

    /// Opens the box for `compose`, on the draft its target has if there is one.
    pub fn open_compose(&mut self, mut compose: Compose) {
        if let Some(draft) = self.drafts.get(&(compose.pr, compose.target.clone())) {
            compose.input = crate::text_input::TextInput::with_text(draft, true);
        }
        // Closed while its words were on their way: it opens still waiting for them.
        let key = Some((compose.pr, compose.target.clone()));
        if let Some(ticket) = self
            .writes
            .iter()
            .find(|(_, k)| **k == key)
            .map(|(t, _)| *t)
        {
            compose.state = Sending::Sending(ticket);
        }
        self.overlays.push(Overlay::Compose(compose));
    }

    /// Keys in the box: Enter sends under a new ticket, Esc keeps the words.
    fn compose_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Compose(c)) = self.overlays.last_mut() else {
            return vec![];
        };
        match c.key(key) {
            ComposeAction::None => vec![],
            ComposeAction::Close => {
                let key = (c.pr, c.target.clone());
                let text = c.input.text().to_string();
                let sending = matches!(c.state, Sending::Sending(_));
                self.overlays.pop();
                if !sending {
                    if text.trim().is_empty() {
                        self.drafts.remove(&key);
                    } else {
                        self.drafts.insert(key, text);
                    }
                }
                vec![]
            }
            ComposeAction::Send(write) => {
                self.next_ticket += 1;
                let ticket = self.next_ticket;
                c.state = Sending::Sending(ticket);
                let pr = c.pr;
                let key = (pr, c.target.clone());
                self.drafts.insert(key.clone(), c.input.text().to_string());
                self.writes.insert(ticket, Some(key));
                vec![Action::Send(ClientRequest::WritePr { pr, ticket, write })]
            }
        }
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
            Some(Overlay::Target(_)) => self.target_key(key),
            Some(Overlay::Worktrees(_)) => self.worktrees_key(key),
            Some(Overlay::FollowUp { .. }) => self.follow_up_key(key),
            Some(Overlay::Hand { .. }) => self.hand_key(key),
            Some(Overlay::Rename { .. }) => self.rename_key(key),
            Some(Overlay::Palette(_)) => self.palette_key(key),
            Some(Overlay::Presets { .. }) => self.presets_key(key),
            Some(Overlay::PresetName { .. }) => self.preset_name_key(key),
            Some(Overlay::Finder(_)) => self.finder_key(key, Instant::now()),
            Some(Overlay::OpenProject(_)) => self.open_project_key(key),
            Some(Overlay::Help { .. }) => {
                self.help_key(key);
                vec![]
            }
            Some(Overlay::Settings(_)) => self.settings_key(key),
            Some(Overlay::Keys(_)) => self.keys_key(key),
            Some(Overlay::KeyCapture(_)) => self.capture_key(key),
            Some(Overlay::Repos { .. }) => self.repos_key(key),
            Some(Overlay::RepoAccount { .. }) => self.repo_account_key(key),
            Some(Overlay::Compose(_)) => self.compose_key(key),
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

    /// `p` and `C-a p`: opens on the last launch line (or the configured CLI, or the
    /// first installed one), and
    /// asks for the prompt history and for CLIs installed since the daemon started.
    fn open_quick_prompt(&mut self) -> Vec<Action> {
        let Some(project) = self.project else {
            self.message = Some("no project open".into());
            return vec![];
        };
        let text = self.prompt_draft.clone().unwrap_or_default();
        // A task of its own: what it sends is not review threads.
        self.hand_for = None;
        // Where the selection is: its worktree, else the project's folder.
        let target = self.empty_worktree().or_else(|| self.card_worktree());
        let actions = self.quick_prompt_in(project, &text, target.clone());
        if target.is_none()
            && self.config.agents.new_worktree_by_default
            && let Some(new) = self.new_worktree()
            && let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut()
        {
            q.new_worktree = Some(new);
        }
        actions
    }

    /// `Ctrl+T`: the project's folder, its worktrees (hidden ones too), and a new
    /// worktree in each repo.
    fn target_picker(&self, project: ProjectId) -> ListPicker<overlay::TargetChoice> {
        use overlay::TargetChoice;
        let name = self
            .state
            .projects
            .iter()
            .find(|p| p.id == project)
            .map_or_else(String::new, |p| p.name.clone());
        let repos = self
            .prs
            .get(&project)
            .map(|d| d.repos.as_slice())
            .unwrap_or(&[]);
        let several = repos.len() > 1;
        let worktrees = self
            .worktrees
            .get(&project)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut items = vec![TargetChoice::Folder];
        let mut labels = vec![format!("{name} · the project's folder")];
        for w in worktrees {
            let branch = w.branch.clone().unwrap_or_else(|| {
                w.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let repo = w
                .repo
                .filter(|_| several)
                .and_then(|id| repos.iter().find(|r| r.repo == id))
                .map(|r| format!("{}@", r.name))
                .unwrap_or_default();
            let stat = w
                .stat
                .filter(|s| s.files > 0 || s.dirty)
                .map(|s| {
                    let dirty = if s.dirty { " ●" } else { "" };
                    format!(" · {} files +{} −{}{dirty}", s.files, s.added, s.removed)
                })
                .unwrap_or_default();
            labels.push(format!("⎇ {repo}{branch}{stat}"));
            items.push(TargetChoice::Worktree(w.path.clone(), branch));
        }
        if several {
            for r in repos {
                labels.push(format!("new worktree in {}", r.name));
                items.push(TargetChoice::New(r.repo, Some(r.name.clone())));
            }
        } else if let Some(new) = self.new_worktree() {
            labels.push("new worktree…".into());
            items.push(TargetChoice::New(new.repo, None));
        }
        let named: HashMap<TargetChoice, String> = items.iter().cloned().zip(labels).collect();
        ListPicker::new(items, move |c| named[c].clone(), true)
    }

    fn target_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Target(picker)) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        if picker.key(key) != Pick::Chosen {
            return vec![];
        }
        let choice = picker.selected().cloned();
        self.overlays.pop();
        let seed = termist_core::now_ms();
        if let (Some(choice), Some(Overlay::QuickPrompt(q))) = (choice, self.overlays.last_mut()) {
            match choice {
                overlay::TargetChoice::Folder => {
                    q.worktree = None;
                    q.new_worktree = None;
                }
                overlay::TargetChoice::Worktree(path, branch) => {
                    q.worktree = Some((path, branch));
                    q.new_worktree = None;
                }
                overlay::TargetChoice::New(repo, repo_name) => {
                    q.worktree = None;
                    q.new_worktree = Some(overlay::NewWorktree {
                        repo,
                        repo_name,
                        seed,
                        name_from: q.issue.as_ref().map(|i| i.words()),
                    });
                }
            }
        }
        vec![]
    }

    /// `X`: asks before removing the worktree at `path`; a live card in it says no.
    fn ask_remove(&mut self, path: PathBuf) {
        let live = self.state.sessions.iter().any(|s| {
            !s.archived
                && s.status.is_live()
                && s.place
                    .as_deref()
                    .map_or(&s.cwd, |p| &p.root)
                    .starts_with(&path)
        });
        if live {
            self.message = Some("stop its cards first (d)".into());
            return;
        }
        self.removing = Some(path);
        self.mode = Mode::ConfirmRemove { files: 0 };
    }

    /// The worktree `X` asks about, by its branch (or folder).
    pub fn removing_name(&self) -> String {
        let Some(path) = &self.removing else {
            return String::new();
        };
        self.project_worktrees()
            .iter()
            .find(|w| &w.path == path)
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| short(path))
    }

    /// `W`: the project's worktrees (opened again, on the same row, when they change).
    fn open_worktrees(&mut self) {
        let Some(project) = self.project else {
            return;
        };
        let at = match self.overlays.last() {
            Some(Overlay::Worktrees(p)) => {
                let at = p.selected_index();
                self.overlays.pop();
                at
            }
            _ => None,
        };
        let repos = self
            .prs
            .get(&project)
            .map(|d| d.repos.as_slice())
            .unwrap_or(&[]);
        let carded: Vec<&Path> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.project == project && !s.archived)
            .map(|s| {
                s.place
                    .as_deref()
                    .map_or(s.cwd.as_path(), |p| p.root.as_path())
            })
            .collect();
        let labels: HashMap<PathBuf, String> = self
            .project_worktrees()
            .iter()
            .map(|w| {
                let cards = carded.contains(&w.path.as_path());
                let mark = if cards || w.shown { "✓" } else { " " };
                let repo = w
                    .repo
                    .filter(|_| repos.len() > 1)
                    .and_then(|id| repos.iter().find(|r| r.repo == id))
                    .map(|r| format!("{}@", r.name))
                    .unwrap_or_default();
                let name = w.branch.clone().unwrap_or_else(|| short(&w.path));
                let who = if w.made_by_termist {
                    "termist"
                } else {
                    "outside"
                };
                let stat = w
                    .stat
                    .filter(|s| s.files > 0 || s.dirty)
                    .map(|s| {
                        let dirty = if s.dirty { " ●" } else { "" };
                        format!(" · {} files +{} −{}{dirty}", s.files, s.added, s.removed)
                    })
                    .unwrap_or_default();
                let end = match w.pr_end {
                    Some((n, termist_core::PrEnd::Merged)) => format!(" · #{n} merged"),
                    Some((n, termist_core::PrEnd::Closed)) => format!(" · #{n} closed"),
                    None => String::new(),
                };
                let cards = if cards { " · has cards" } else { "" };
                (
                    w.path.clone(),
                    format!("{mark} ⎇ {repo}{name} · {who}{stat}{end}{cards}"),
                )
            })
            .collect();
        let items: Vec<PathBuf> = self
            .project_worktrees()
            .iter()
            .map(|w| w.path.clone())
            .collect();
        let mut picker = ListPicker::new(items, move |p| labels[p].clone(), false);
        if let Some(at) = at {
            picker.select_index(at);
        }
        self.overlays.push(Overlay::Worktrees(picker));
    }

    fn worktrees_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Worktrees(picker)) = self.overlays.last_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => {
                self.overlays.pop();
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let Some(path) = picker.selected().cloned() else {
                    return vec![];
                };
                let has_cards = self.state.sessions.iter().any(|s| {
                    !s.archived && s.place.as_deref().map_or(&s.cwd, |p| &p.root) == &path
                });
                if has_cards {
                    self.message = Some("it has cards: always shown".into());
                    return vec![];
                }
                let shown = self
                    .project_worktrees()
                    .iter()
                    .any(|w| w.path == path && w.shown);
                return vec![Action::Send(ClientRequest::SetWorktreeShown {
                    path,
                    shown: !shown,
                })];
            }
            KeyCode::Char('X') => {
                let Some(path) = picker.selected().cloned() else {
                    return vec![];
                };
                self.overlays.pop();
                self.ask_remove(path);
            }
            _ => {
                picker.key(key);
            }
        }
        vec![]
    }

    /// `Shift+P`: a new task like the selected card's: its CLI, model and effort, in its
    /// worktree.
    fn same_task(&mut self) -> Vec<Action> {
        let Some(info) = self.selected_info().cloned() else {
            return self.open_quick_prompt();
        };
        let SessionKind::Agent { harness } = info.kind else {
            self.message = Some("a shell has no agent to start again".into());
            return vec![];
        };
        self.hand_for = None;
        let target = self.card_worktree();
        let actions = self.quick_prompt_in(info.project, "", target);
        if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
            q.launch = LaunchOptions {
                harness,
                model: info.model.clone(),
                effort: info.effort.clone(),
            };
        }
        actions
    }

    /// The selected card's worktree, when it is not in the project's own folder.
    fn card_worktree(&self) -> Option<(PathBuf, String)> {
        let id = self.selected?;
        let s = self.state.sessions.iter().find(|s| s.id == id)?;
        let place = s.place.as_deref()?;
        let project = self.state.projects.iter().find(|p| p.id == s.project)?;
        if project.path.starts_with(&place.root) || place.gone {
            return None;
        }
        let name = place.branch.clone().unwrap_or_else(|| {
            place
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        Some((place.root.clone(), name))
    }

    /// A new worktree in the selected card's repo, else the project's first; `None`
    /// before any repo of the project is known.
    fn new_worktree(&self) -> Option<overlay::NewWorktree> {
        let project = self.project?;
        let repos = self
            .prs
            .get(&project)
            .map(|d| d.repos.as_slice())
            .unwrap_or(&[]);
        let selected = self
            .selected
            .and_then(|id| self.state.sessions.iter().find(|s| s.id == id))
            .and_then(|s| s.place.as_ref()?.repo);
        let repo = selected
            .or_else(|| repos.first().map(|r| r.repo))
            .or_else(|| self.project_worktrees().iter().find_map(|w| w.repo))?;
        let repo_name = (repos.len() > 1)
            .then(|| {
                repos
                    .iter()
                    .find(|r| r.repo == repo)
                    .map(|r| r.name.clone())
            })
            .flatten();
        Some(overlay::NewWorktree {
            repo,
            repo_name,
            seed: termist_core::now_ms(),
            name_from: None,
        })
    }

    /// A new worktree for `issue`: in its repo, on a branch named after it.
    fn issue_worktree(
        &self,
        project: ProjectId,
        issue: &overlay::FromIssue,
    ) -> overlay::NewWorktree {
        let repos = self
            .prs
            .get(&project)
            .map(|d| d.repos.as_slice())
            .unwrap_or(&[]);
        let repo_name = (repos.len() > 1)
            .then(|| {
                repos
                    .iter()
                    .find(|r| r.repo == issue.repo)
                    .map(|r| r.name.clone())
            })
            .flatten();
        overlay::NewWorktree {
            repo: issue.repo,
            repo_name,
            seed: termist_core::now_ms(),
            name_from: Some(issue.words()),
        }
    }

    /// `Enter` on an issue: the quick prompt with the issue in it, on a new worktree
    /// named after it.
    fn start_from_issue(&mut self, at: prs::issues::IssueRef) -> Vec<Action> {
        let Some(project) = self.project else {
            return vec![];
        };
        let Some((repo, issue)) = self.issues.get(&project).and_then(|d| {
            let repo = d.repos.iter().find(|r| r.repo == at.repo)?;
            Some((repo, repo.issues.iter().find(|i| i.number == at.number)?))
        }) else {
            return vec![];
        };
        let text = prs::issues::prompt(&repo.slug, issue);
        let from = overlay::FromIssue {
            link: termist_core::IssueLink {
                number: issue.number,
                url: issue.url.clone(),
            },
            title: issue.title.clone(),
            repo: repo.repo,
        };
        // A task of its own: what it sends is not review threads.
        self.hand_for = None;
        let actions = self.quick_prompt_in(project, &text, None);
        let new = self.issue_worktree(project, &from);
        if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
            q.new_worktree = Some(new);
            q.issue = Some(Box::new(from));
        }
        actions
    }

    /// The branch a new worktree would get for `q` now.
    pub fn new_branch(&self, q: &QuickPrompt) -> Option<String> {
        let new = q.new_worktree.as_ref()?;
        let taken: Vec<String> = self
            .worktrees
            .get(&q.project)
            .into_iter()
            .flatten()
            .filter_map(|w| w.branch.clone())
            .collect();
        let words = new.name_from.as_deref().unwrap_or(q.input.text());
        Some(crate::slug::branch(words, &taken, new.seed))
    }

    /// The quick prompt for `project` with `text` in it, for its folder or a worktree.
    fn quick_prompt_in(
        &mut self,
        project: ProjectId,
        text: &str,
        worktree: Option<(PathBuf, String)>,
    ) -> Vec<Action> {
        let launch = self
            .state
            .last_launch
            .clone()
            .filter(|l| self.is_available(l.harness))
            .unwrap_or_else(|| LaunchOptions {
                harness: Some(self.config.agents.default)
                    .filter(|h| self.is_available(*h))
                    .or_else(|| {
                        self.harnesses
                            .iter()
                            .find(|h| h.available)
                            .map(|h| h.harness)
                    })
                    .unwrap_or(Harness::Claude),
                model: None,
                effort: None,
            });
        let mut input = TextInput::with_text(text, true);
        input.set_history(self.prompt_history.clone());
        self.overlays.push(Overlay::QuickPrompt(QuickPrompt {
            input,
            project,
            launch,
            worktree,
            new_worktree: None,
            preset: None,
            issue: None,
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
                if let Some(Overlay::QuickPrompt(q)) = self.overlays.pop() {
                    self.keep_draft(&q);
                }
                self.hand_for = None;
            }
            KeyCode::Tab => {
                let current = q.launch.harness;
                let mut picker = overlay::harness_picker(&self.harnesses);
                if let Some(i) = self.harnesses.iter().position(|h| h.harness == current) {
                    picker.select_index(i);
                }
                self.overlays.push(Overlay::Harness(picker));
            }
            KeyCode::Char('s') if ctrl => {
                // The words before the cursor go first, the rest after what is typed.
                let (prefix, postfix) = q.input.split_at_cursor();
                let preset = Preset {
                    name: String::new(),
                    harness: q.launch.harness,
                    model: q.launch.model.clone(),
                    effort: q.launch.effort.clone(),
                    prefix: prefix.to_string(),
                    postfix: postfix.to_string(),
                    local: false,
                };
                let name = q
                    .preset
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.overlays.push(Overlay::PresetName {
                    input: TextInput::with_text(&name, false),
                    name_for: overlay::NameFor::Save(preset),
                    replace: false,
                });
            }
            KeyCode::Char('o') if ctrl => {
                let launch = q.launch.clone();
                let recent = self
                    .recent_models
                    .get(&launch.harness)
                    .cloned()
                    .unwrap_or_default();
                let catalog = self
                    .model_catalogs
                    .get(&launch.harness)
                    .cloned()
                    .unwrap_or_default();
                self.overlays
                    .push(Overlay::Model(ModelPicker::new(&launch, recent, &catalog)));
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
            KeyCode::Char('n') if ctrl => {
                let was = q.new_worktree.take().is_some();
                let issue = q.issue.clone();
                if !was {
                    let project = q.project;
                    let new = match &issue {
                        Some(issue) => Some(self.issue_worktree(project, issue)),
                        None => self.new_worktree(),
                    };
                    match new {
                        Some(new) => {
                            if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
                                q.new_worktree = Some(new);
                                q.worktree = None;
                            }
                        }
                        None => self.message = Some("no repo known yet in this project".into()),
                    }
                }
            }
            KeyCode::Char('t') if ctrl => {
                let project = q.project;
                let picker = self.target_picker(project);
                self.overlays.push(Overlay::Target(picker));
            }
            _ => {
                if q.input.key(key) == Edit::Submit {
                    return self.submit_quick_prompt();
                }
            }
        }
        vec![]
    }

    /// A quick prompt closed without starting: its text waits for the next, unless it is
    /// blank or an earlier prompt recalled and left as it was (`↑` brings that back).
    fn keep_draft(&mut self, q: &QuickPrompt) {
        if q.worktree.is_some() || q.issue.is_some() {
            return; // words for a pull request's worktree or an issue, not a draft for `p`
        }
        let text = q.input.text();
        let own = !text.trim().is_empty() && !q.input.is_from_history();
        self.prompt_draft = own.then(|| text.to_string());
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
        // A new worktree first; the task starts in it when it is made.
        if let (Some(new), Some(branch)) = (q.new_worktree.clone(), self.new_branch(&q)) {
            self.next_ticket += 1;
            let ticket = self.next_ticket;
            self.message = Some(format!("making a worktree {branch}…"));
            let project = q.project;
            self.task_for.insert(ticket, q);
            return vec![Action::Send(ClientRequest::CreateWorktree {
                project,
                repo: new.repo,
                branch,
                ticket,
            })];
        }
        self.start_task(q)
    }

    /// Starts the quick prompt's task where it says.
    fn start_task(&mut self, q: QuickPrompt) -> Vec<Action> {
        let harness = q.launch.harness;
        let text = q.input.text();
        let prompt = (!text.trim().is_empty()).then(|| text.to_string());
        self.prompt_draft = None;
        if let Some(pr) = self.hand_for.take() {
            self.marks.remove(&pr);
        }
        // The daemon stores it without sending the state again: keep our copy current.
        self.state.last_launch = Some(q.launch.clone());
        self.focus_next_created = true;
        let (cols, rows) = self.pane;
        vec![
            Action::Send(ClientRequest::SetLastLaunch(q.launch.clone())),
            Action::Send(ClientRequest::CreateSession {
                project: q.project,
                kind: SessionKind::Agent { harness },
                cwd: q.worktree.clone().map(|(path, _)| path),
                prompt,
                issue: q.issue.as_ref().map(|i| i.link.url.clone()),
                title_from: q.title_from(),
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
            KeyCode::Esc => {
                self.overlays.pop();
            }
            KeyCode::Left => m.step_effort(-1),
            KeyCode::Right => m.step_effort(1),
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

    fn open_repos(&mut self) -> Vec<Action> {
        let Some(project) = self.project else {
            return vec![];
        };
        if !self.config.github.enabled {
            self.message = Some("pull requests are off".into());
            return vec![];
        }
        let repos = self
            .repo_lists
            .get(&project)
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        self.overlays.push(Overlay::Repos {
            project,
            picker: ListPicker::new(repos, overlay::repo_label, false),
        });
        vec![Action::Send(ClientRequest::ListRepos { project })]
    }

    fn repos_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Repos { project, picker }) = self.overlays.last_mut() else {
            return vec![];
        };
        let project = *project;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
            }
            KeyCode::Char(' ') => {
                let Some(i) = picker.selected_index() else {
                    return vec![];
                };
                let mut items = picker.items().to_vec();
                items[i].visible = !items[i].visible;
                let (repo, visible) = (items[i].id, items[i].visible);
                picker.set_items(items, overlay::repo_label);
                return vec![Action::Send(ClientRequest::SetRepoVisible {
                    repo,
                    visible,
                })];
            }
            KeyCode::Char('a') => {
                let Some(repo) = picker.selected().map(|r| r.id) else {
                    return vec![];
                };
                let accounts = self
                    .repo_lists
                    .get(&project)
                    .map(|(a, _)| a.clone())
                    .unwrap_or_default();
                let mut items = vec![None];
                items.extend(accounts.into_iter().map(Some));
                self.overlays.push(Overlay::RepoAccount {
                    repo,
                    picker: ListPicker::new(
                        items,
                        |a: &Option<String>| a.clone().unwrap_or_default(),
                        false,
                    ),
                });
            }
            _ => {
                picker.key(key);
            }
        }
        vec![]
    }

    fn repo_account_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::RepoAccount { repo, picker }) = self.overlays.last_mut() else {
            return vec![];
        };
        let repo = *repo;
        match picker.key(key) {
            Pick::Chosen => {
                let account = picker.selected().cloned().flatten();
                self.overlays.pop();
                vec![Action::Send(ClientRequest::SetRepoAccount {
                    repo,
                    account,
                })]
            }
            Pick::Ignored if key.code == KeyCode::Esc => {
                self.overlays.pop();
                vec![]
            }
            _ => vec![],
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
            if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut()
                && q.project != id
            {
                q.project = id;
                q.worktree = None; // the worktree was the other project's
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
        self.hand_for = None; // its own words, not review threads
        self.overlays.push(Overlay::FollowUp {
            session,
            input: TextInput::new(true),
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
            self.hand_for = None;
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
        // Review threads handed over: their marks are done.
        if let Some(pr) = self.hand_for.take() {
            self.marks.remove(&pr);
        }
        let modes = self
            .screens
            .get(&session)
            .map(|s| s.modes)
            .unwrap_or_default();
        if self.attached == Some(session) {
            self.scrolling = false;
            self.copy = None;
        }
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
            self.project_pending = None;
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
            // The daemon stores a project under its real path: a link to a project
            // that is open already just brings its tab forward.
            (KeyCode::Tab, Some(BrowseEntry::Dir(d))) => {
                self.overlays.pop();
                let open = self.open_projects().find(|p| p.path == d.canonical);
                if let Some(id) = open.map(|p| p.id) {
                    self.go_to_project(id);
                    return vec![];
                }
                self.project_pending = Some(ProjectPending::Path(d.canonical.clone()));
                return vec![Action::Send(ClientRequest::AddProject {
                    path: d.canonical,
                })];
            }
            (KeyCode::Enter | KeyCode::Tab, Some(BrowseEntry::Project(p))) => {
                self.overlays.pop();
                if p.open {
                    self.go_to_project(p.id);
                    return vec![];
                }
                self.project_pending = Some(ProjectPending::Known {
                    project: p.id,
                    select: None,
                });
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

    /// `.` / `,` also reach an agent waiting in a closed project: its project is opened
    /// and the card selected when it arrives.
    fn navigate(&mut self, c: char) -> Vec<Action> {
        match c {
            '.' | ',' => {
                // A card on its way counts as where you are; going on from it, you
                // no longer wait for it.
                let from = match self.project_pending.take() {
                    Some(ProjectPending::Known {
                        select: Some(id), ..
                    }) => Some(id),
                    _ => self.selected,
                };
                let waiting = self.waiting_in_closed_projects();
                let mut reachable = self.visible_sessions();
                reachable.extend(waiting.iter().cloned());
                let Some(id) = next_in_attention(&reachable, from, c == '.') else {
                    return vec![];
                };
                return self.reveal(id);
            }
            'h' => self.move_by(-1),
            'l' => self.move_by(1),
            'j' => self.move_rows(1),
            'k' => self.move_rows(-1),
            _ => {}
        }
        vec![]
    }

    /// Shows card `id`: opens its project first if it is closed, and leaves the archive
    /// view (which would not keep it selected).
    fn reveal(&mut self, id: SessionId) -> Vec<Action> {
        let Some(s) = self.state.sessions.iter().find(|s| s.id == id) else {
            return vec![];
        };
        let project = s.project;
        self.view = View::Grid;
        if self.open_projects().any(|p| p.id == project) {
            self.select(id);
            return vec![];
        }
        self.project_pending = Some(ProjectPending::Known {
            project,
            select: Some(id),
        });
        vec![Action::Send(ClientRequest::OpenProject { project })]
    }

    /// Opens pull request `pr` of `project` in the PR view.
    fn reveal_pr(&mut self, project: ProjectId, pr: PrRef) -> Vec<Action> {
        if !self.open_projects().any(|p| p.id == project) {
            return vec![];
        }
        // Out of scroll-back first: the pane it scrolled may be left behind.
        let mut actions = self.stop_scrolling();
        self.go_to_project(project);
        self.mode = Mode::Grid;
        let mut view = PrView::for_project(Some(project));
        view.selected = Some(pr);
        let summary = self
            .prs
            .get(&project)
            .and_then(|d| d.repos.iter().find(|r| r.repo == pr.repo))
            .and_then(|r| r.prs.iter().find(|p| p.number == pr.number))
            .cloned();
        if let Some(s) = summary {
            actions.push(Action::Send(ClientRequest::MarkPrSeen {
                pr,
                updated_at: s.updated_at.clone(),
            }));
            view.detail = Some(prs::Detail::new(pr, s));
        }
        self.view = View::Prs(view);
        actions.extend(self.sync_attachment());
        actions.extend(self.sync_prs());
        actions
    }

    /// The wheel moves through the inbox or scrolls the detail; a click selects a row,
    /// a click on the selected row opens it.
    /// The wheel moves through the issues; a click selects one, and on the selected
    /// one is `Enter`.
    fn issues_mouse(&mut self, ev: MouseEvent, layout: &PrLayout) -> Vec<Action> {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        match ev.kind {
            MouseEventKind::ScrollDown => self.prs_key(press(KeyCode::Down)),
            MouseEventKind::ScrollUp => self.prs_key(press(KeyCode::Up)),
            MouseEventKind::Down(MouseButton::Left) => {
                let inside = ev.column >= layout.list.x
                    && ev.column < layout.list.right()
                    && ev.row >= layout.list.y
                    && ev.row < layout.list.bottom();
                if !inside {
                    return vec![];
                }
                let index = layout.first + (ev.row - layout.list.y) as usize;
                let none = ProjectIssues::default();
                let data = self
                    .project
                    .and_then(|p| self.issues.get(&p))
                    .unwrap_or(&none);
                let View::Prs(view) = &mut self.view else {
                    return vec![];
                };
                let Some(issue) = prs::issues::rows(data, &view.issues)
                    .get(index)
                    .and_then(|r| r.issue_ref())
                else {
                    return vec![];
                };
                if view.issues.selected == Some(issue) {
                    return self.prs_key(press(KeyCode::Enter));
                }
                view.issues.selected = Some(issue);
                let list = prs::issues::rows(data, &view.issues);
                view.issues.repair(&list);
                vec![]
            }
            _ => vec![],
        }
    }

    fn prs_mouse(&mut self, ev: MouseEvent) -> Vec<Action> {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let layout = self.pr_layout.borrow().clone();
        let View::Prs(view) = &mut self.view else {
            return vec![];
        };
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && ev.row == layout.section_row
            && let Some((section, ..)) = layout
                .sections
                .iter()
                .find(|(_, a, b)| (*a..*b).contains(&ev.column))
        {
            view.section = *section;
            return vec![];
        }
        if view.section == Section::Issues {
            return self.issues_mouse(ev, &layout);
        }
        let diff = view
            .detail
            .as_ref()
            .and_then(|d| self.pr_diffs.get(&d.pr))
            .and_then(|(_, d)| d.as_ref());
        if let MouseEventKind::ScrollDown | MouseEventKind::ScrollUp = ev.kind
            && let Some(open) = view.detail.as_mut().and_then(|d| d.diff.as_mut())
        {
            let down = ev.kind == MouseEventKind::ScrollDown;
            open.wheel(
                ev.column,
                ev.row,
                down,
                diff.map(|d| d.files.as_slice()),
                &layout.diff,
            );
            return vec![];
        }
        if ev.kind == MouseEventKind::Down(MouseButton::Left) && view.detail.is_some() {
            view.click(ev.column, ev.row, diff, &layout);
            return vec![];
        }
        match ev.kind {
            MouseEventKind::ScrollDown => self.prs_key(press(KeyCode::Down)),
            MouseEventKind::ScrollUp => self.prs_key(press(KeyCode::Up)),
            MouseEventKind::Down(MouseButton::Left) => {
                let View::Prs(view) = &mut self.view else {
                    return vec![];
                };
                let inside = ev.column >= layout.list.x
                    && ev.column < layout.list.right()
                    && ev.row >= layout.list.y
                    && ev.row < layout.list.bottom();
                if view.detail.is_some() || !inside {
                    return vec![];
                }
                let index = layout.first + (ev.row - layout.list.y) as usize;
                let empty = ProjectPrs::default();
                let data = self
                    .project
                    .and_then(|p| self.prs.get(&p))
                    .unwrap_or(&empty);
                let Some(pr) = prs::rows(data, view).get(index).and_then(|r| r.pr_ref()) else {
                    return vec![];
                };
                if view.selected == Some(pr) {
                    return self.prs_key(press(KeyCode::Enter));
                }
                view.selected = Some(pr);
                let list = prs::rows(data, view);
                view.repair(&list);
                vec![]
            }
            _ => vec![],
        }
    }

    fn move_by(&mut self, delta: isize) {
        // In the order drawn: band by band.
        let slots: Vec<Slot> = self.grid_rows().into_iter().flat_map(|r| r.slots).collect();
        if slots.is_empty() {
            return;
        }
        let here = self.slot();
        let pos = here
            .and_then(|s| slots.iter().position(|x| *x == s))
            .unwrap_or(0) as isize;
        let next = (pos + delta).clamp(0, slots.len() as isize - 1) as usize;
        self.set_slot(slots[next].clone());
    }

    /// The selected band stand-in's worktree, with its branch (or folder) name.
    pub fn empty_worktree(&self) -> Option<(PathBuf, String)> {
        let path = self.empty.clone().filter(|_| self.selected.is_none())?;
        let w = self.project_worktrees().iter().find(|w| w.path == path)?;
        let name = w.branch.clone().unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        Some((path, name))
    }

    /// Enter on a band's stand-in: the quick prompt, for its worktree.
    fn start_in_empty(&mut self) -> Vec<Action> {
        let (Some(project), Some(worktree)) = (self.project, self.empty_worktree()) else {
            return vec![];
        };
        self.quick_prompt_in(project, "", Some(worktree))
    }

    /// What is selected in the grid: a card, else a band's stand-in.
    pub fn slot(&self) -> Option<Slot> {
        match (self.selected, &self.empty) {
            (Some(id), _) => Some(Slot::Card(id)),
            (None, Some(path)) => Some(Slot::Empty(path.clone())),
            (None, None) => None,
        }
    }

    fn set_slot(&mut self, slot: Slot) {
        match slot {
            Slot::Card(id) => {
                self.selected = Some(id);
                self.empty = None;
            }
            Slot::Empty(path) => {
                self.selected = None;
                self.empty = Some(path);
            }
        }
    }

    /// The current project's worktrees.
    pub fn project_worktrees(&self) -> &[termist_core::WorktreeInfo] {
        self.project
            .and_then(|p| self.worktrees.get(&p))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Seconds since 1970, for the ages of pull requests.
    pub fn now_secs(&self) -> i64 {
        self.frozen_now
            .unwrap_or_else(|| (termist_core::now_ms() / 1000) as i64)
    }

    /// `v` and `i`: the view on `section`; on it already, back to the grid.
    fn toggle_prs(&mut self, section: Section) {
        self.mode = Mode::Grid;
        if let View::Prs(view) = &mut self.view {
            if view.section == section {
                self.view = View::Grid;
            } else {
                view.section = section;
            }
            return;
        }
        // On the selected card's pull request, when its branch has one.
        let mut view = PrView::for_project(self.project);
        view.selected = self.card_pr();
        view.section = section;
        self.view = View::Prs(view);
    }

    /// Asks the daemon for `pr`'s worktree; the quick prompt opens there with `text`
    /// (or the pull request's own line) when it is ready.
    fn ask_worktree(&mut self, pr: PrRef, text: Option<String>) -> Vec<Action> {
        self.worktree_for = Some((pr, text));
        self.message = Some(format!("opening a worktree for #{}…", pr.number));
        vec![Action::Send(ClientRequest::OpenWorktree { pr })]
    }

    /// The pull request as the inbox last read it, with its project.
    fn pr_summary(&self, pr: PrRef) -> Option<(ProjectId, &termist_core::github::PrSummary)> {
        self.prs.iter().find_map(|(project, data)| {
            let summary = data
                .repos
                .iter()
                .find(|r| r.repo == pr.repo)?
                .prs
                .iter()
                .find(|p| p.number == pr.number)?;
            Some((*project, summary))
        })
    }

    /// The worktree is there: the quick prompt opens in it.
    fn worktree_ready(&mut self, pr: PrRef, path: PathBuf, text: Option<String>) -> Vec<Action> {
        self.message = None;
        let Some((project, summary)) = self.pr_summary(pr) else {
            return vec![];
        };
        let branch = summary.head.clone();
        let text = text.unwrap_or_else(|| {
            format!(
                "Pull request #{} \"{}\" ({}): {} into {}. ",
                pr.number, summary.title, summary.url, summary.head, summary.base
            )
        });
        self.quick_prompt_in(project, &text, Some((path, branch)))
    }

    /// `a`: the marked threads of `pr` (else the one at `here`) as words for an agent:
    /// to a live card on its branch, or to a new agent in its worktree.
    fn hand(&mut self, pr: PrRef, here: Option<String>) -> Vec<Action> {
        let Some(detail) = self.pr_details.get(&pr).and_then(|(_, d)| d.as_ref()) else {
            self.message = Some("not loaded yet".into());
            return vec![];
        };
        let threads: Vec<&termist_core::github::Thread> =
            match self.marks.get(&pr).filter(|m| !m.is_empty()) {
                Some(marked) => detail
                    .threads
                    .iter()
                    .filter(|th| marked.contains(&th.id))
                    .collect(),
                None => match here.and_then(|id| detail.threads.iter().find(|th| th.id == id)) {
                    None => {
                        self.message = Some("mark threads with Space first".into());
                        return vec![];
                    }
                    Some(th) if th.resolved => {
                        self.message = Some("resolved: mark it with Space to send it".into());
                        return vec![];
                    }
                    Some(th) => vec![th],
                },
            };
        let branch = detail.summary.head.clone();
        let text = crate::prs::hand::text(pr.number, &branch, &threads);
        let cards: Vec<&SessionInfo> = self
            .state
            .sessions
            .iter()
            .filter(|s| {
                !s.archived
                    && matches!(s.kind, SessionKind::Agent { .. })
                    && s.status.is_live()
                    && s.place.as_ref().is_some_and(|p| p.pr == Some(pr))
            })
            .collect();
        self.hand_for = Some(pr);
        if cards.is_empty() {
            return self.ask_worktree(pr, Some(text));
        }
        let labels: HashMap<HandTo, String> = cards
            .iter()
            .map(|s| {
                let (_, _, word) = crate::ui::status_style(&self.theme, s.status);
                (
                    HandTo::Card(s.id),
                    format!("{} · {} · {word}", s.kind.label(), s.display_name()),
                )
            })
            .chain([(HandTo::New, format!("new agent in ⎇ {branch}"))])
            .collect();
        let mut items: Vec<HandTo> = cards.iter().map(|s| HandTo::Card(s.id)).collect();
        items.push(HandTo::New);
        let picker = ListPicker::new(items, move |to| labels[to].clone(), false);
        self.overlays.push(Overlay::Hand { pr, text, picker });
        vec![]
    }

    fn hand_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Hand { picker, .. }) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            self.hand_for = None;
            return vec![];
        }
        if picker.key(key) != Pick::Chosen {
            return vec![];
        }
        let Some(Overlay::Hand { pr, text, picker }) = self.overlays.pop() else {
            return vec![];
        };
        match picker.selected().cloned() {
            Some(HandTo::Card(session)) => {
                if let Some(why) = self.follow_up_refused(session) {
                    self.message = Some(why.into());
                    self.hand_for = None;
                    return vec![];
                }
                self.overlays.push(Overlay::FollowUp {
                    session,
                    input: TextInput::with_text(&text, true),
                });
                vec![]
            }
            Some(HandTo::New) => self.ask_worktree(pr, Some(text)),
            None => vec![],
        }
    }

    /// The open pull request of the selected card's branch.
    pub fn card_pr(&self) -> Option<PrRef> {
        let id = self.selected?;
        let s = self.state.sessions.iter().find(|s| s.id == id)?;
        s.place.as_ref()?.pr
    }

    /// `Shift+V`: the selected card's pull request in the browser.
    fn card_pr_in_browser(&mut self) -> Vec<Action> {
        let url = self.card_pr().and_then(|pr| {
            let repo = self
                .prs
                .values()
                .flat_map(|d| &d.repos)
                .find(|r| r.repo == pr.repo)?;
            Some(
                repo.prs
                    .iter()
                    .find(|p| p.number == pr.number)
                    .map(|p| p.url.clone())
                    .unwrap_or_else(|| {
                        format!("https://github.com/{}/pull/{}", repo.slug, pr.number)
                    }),
            )
        });
        match url {
            Some(url) => vec![Action::OpenUrl(url)],
            None => {
                self.message = Some("no pull request for this branch".into());
                vec![]
            }
        }
    }

    /// Keeps the PR view on the current project's data, and tells the daemon what is
    /// looked at.
    fn sync_prs(&mut self) -> Vec<Action> {
        let project = self.project;
        let focus = match &mut self.view {
            View::Prs(view) => {
                if view.project != project {
                    *view = PrView::for_project(project);
                }
                let empty = ProjectPrs::default();
                let data = project.and_then(|p| self.prs.get(&p)).unwrap_or(&empty);
                let list = prs::rows(data, view);
                view.repair(&list);
                let none = ProjectIssues::default();
                let issues = project.and_then(|p| self.issues.get(&p)).unwrap_or(&none);
                let list = prs::issues::rows(issues, &view.issues);
                view.issues.repair(&list);
                // The pull request open behind the Issues tab is not read meanwhile.
                let detail = view
                    .detail
                    .as_ref()
                    .filter(|_| view.section == Section::Pulls);
                (
                    project,
                    detail.map(|d| d.pr),
                    detail.is_some_and(|d| d.diff.is_some()),
                    view.section == Section::Issues,
                )
            }
            _ => (None, None, false, false),
        };
        if focus == self.pr_focus || !self.config.github.enabled {
            return vec![];
        }
        self.pr_focus = focus;
        vec![Action::Send(ClientRequest::SetPrFocus {
            project: focus.0,
            pr: focus.1,
            diff: focus.2,
            issues: focus.3,
        })]
    }

    /// A key in the PR view. The grid's keys for tabs, help, settings, refresh and
    /// leaving work here too, unless the search takes the keys.
    fn prs_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let section = match &self.view {
            View::Prs(v) => v.section,
            _ => Section::Pulls,
        };
        let diff = match &self.view {
            View::Prs(v) if section == Section::Pulls => {
                v.detail.as_ref().and_then(|d| d.diff.as_ref())
            }
            _ => None,
        };
        let typing = match &self.view {
            View::Prs(v) if section == Section::Issues => v.issues.typing,
            View::Prs(v) => v.typing || diff.is_some_and(|d| d.typing),
            _ => false,
        };
        // In the diff `s` turns it unified or split and `v` chooses lines; the
        // settings and the grid stay a key away.
        let settings = diff.is_none();
        if !typing {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                self.mode = Mode::ConfirmQuit;
                return vec![];
            }
            match self.keymap.action(Context::Grid, &key) {
                Some(KeyAction::Settings | KeyAction::PullRequests) if !settings => {}
                Some(
                    action @ (KeyAction::PullRequests
                    | KeyAction::Issues
                    | KeyAction::RefreshGitHub
                    | KeyAction::NextTab
                    | KeyAction::PrevTab
                    | KeyAction::Tab(_)
                    | KeyAction::Help
                    | KeyAction::Settings
                    | KeyAction::Quit),
                ) => return self.act(action),
                _ => {}
            }
        }
        let View::Prs(view) = &mut self.view else {
            return vec![];
        };
        // `Tab` on a list: the other one.
        if key.code == KeyCode::Tab
            && !typing
            && (section == Section::Issues || view.detail.is_none())
        {
            view.section = section.other();
            return vec![];
        }
        if section == Section::Issues {
            let none = ProjectIssues::default();
            let data = self
                .project
                .and_then(|p| self.issues.get(&p))
                .unwrap_or(&none);
            let layout = self.pr_layout.borrow().clone();
            return match view.issues.key(key, data, &layout) {
                None => vec![],
                Some(IssueAction::Close) => {
                    self.view = View::Grid;
                    vec![]
                }
                Some(IssueAction::Browser(url)) => vec![Action::OpenUrl(url)],
                Some(IssueAction::Repos) => self.open_repos(),
                Some(IssueAction::Start(at)) => self.start_from_issue(at),
            };
        }
        let empty = ProjectPrs::default();
        let data = self
            .project
            .and_then(|p| self.prs.get(&p))
            .unwrap_or(&empty);
        let diff = view
            .detail
            .as_ref()
            .and_then(|d| self.pr_diffs.get(&d.pr))
            .and_then(|(_, d)| d.as_ref());
        let layout = self.pr_layout.borrow().clone();
        match view.key(key, data, diff, &layout) {
            None => vec![],
            Some(PrAction::Close) => {
                self.view = View::Grid;
                vec![]
            }
            Some(PrAction::Opened(pr, updated_at)) => {
                vec![Action::Send(ClientRequest::MarkPrSeen { pr, updated_at })]
            }
            Some(PrAction::Repos) => self.open_repos(),
            Some(PrAction::Worktree) => {
                let pr = match &self.view {
                    View::Prs(v) => v.detail.as_ref().map(|d| d.pr).or(v.selected),
                    _ => None,
                };
                match pr {
                    Some(pr) => self.ask_worktree(pr, None),
                    None => vec![],
                }
            }
            Some(PrAction::Browser(url)) => vec![Action::OpenUrl(url)],
            Some(PrAction::Viewed { pr, path, viewed }) => {
                vec![Action::Send(ClientRequest::SetFileViewed {
                    pr,
                    path,
                    viewed,
                })]
            }
            Some(PrAction::Ask(ask)) => self.ask(ask),
            Some(PrAction::Mark(thread)) => {
                if let View::Prs(view) = &self.view
                    && let Some(pr) = view.detail.as_ref().map(|d| d.pr)
                {
                    let marks = self.marks.entry(pr).or_default();
                    if !marks.remove(&thread) {
                        marks.insert(thread);
                    }
                    if marks.is_empty() {
                        self.marks.remove(&pr);
                    }
                }
                vec![]
            }
            Some(PrAction::Hand(here)) => {
                let pr = match &self.view {
                    View::Prs(view) => view.detail.as_ref().map(|d| d.pr),
                    _ => None,
                };
                match pr {
                    Some(pr) => self.hand(pr, here),
                    None => vec![],
                }
            }
            Some(PrAction::Note(why)) => {
                self.message = Some(why.into());
                vec![]
            }
            Some(PrAction::FlipLayout) => self.flip_layout(),
        }
    }

    /// `e`: the presets; none says how to make one.
    fn open_presets(&mut self) {
        if self.config.presets.is_empty() {
            self.message = Some("no presets · in the new-task prompt, ^S saves one".into());
            return;
        }
        let names = self.config.presets.iter().map(|p| p.name.clone()).collect();
        self.overlays.push(Overlay::Presets {
            picker: ListPicker::new(names, |n: &String| n.clone(), false),
            deleting: None,
        });
    }

    /// A key in the presets: Enter opens the new-task prompt with one, `r` renames,
    /// `d` deletes after asking.
    fn presets_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Presets { picker, deleting }) = self.overlays.last_mut() else {
            return vec![];
        };
        if let Some(name) = deleting.take() {
            self.message = None;
            if key.code != KeyCode::Char('y') {
                return vec![];
            }
            let list: Vec<Preset> = self
                .main_presets()
                .into_iter()
                .filter(|p| p.name != name)
                .collect();
            return self.write_presets(list);
        }
        let chosen = picker.selected().cloned();
        let local = chosen
            .as_ref()
            .and_then(|n| self.config.presets.iter().find(|p| &p.name == n))
            .is_some_and(|p| p.local);
        match key.code {
            KeyCode::Char('r' | 'd') if local => {
                let name = chosen.unwrap_or_default();
                self.message = Some(format!("{name} is set in config.local.toml"));
                vec![]
            }
            KeyCode::Char('r') => {
                if let Some(name) = chosen {
                    self.overlays.push(Overlay::PresetName {
                        input: TextInput::with_text(&name, false),
                        name_for: overlay::NameFor::Rename(name),
                        replace: false,
                    });
                }
                vec![]
            }
            KeyCode::Char('d') => {
                if let Some(name) = &chosen {
                    self.message = Some(format!("delete preset {name}? y/N"));
                }
                if let Some(Overlay::Presets { deleting, .. }) = self.overlays.last_mut() {
                    *deleting = chosen;
                }
                vec![]
            }
            KeyCode::Esc => {
                self.overlays.pop();
                vec![]
            }
            KeyCode::Enter => {
                let chosen = picker.selected().cloned();
                self.overlays.pop();
                let Some(preset) = chosen
                    .and_then(|name| self.config.presets.iter().find(|p| p.name == name).cloned())
                else {
                    return vec![];
                };
                self.prompt_with(preset)
            }
            _ => {
                picker.key(key);
                vec![]
            }
        }
    }

    /// The presets config.toml keeps (config.local.toml's are not written).
    fn main_presets(&self) -> Vec<Preset> {
        self.config
            .presets
            .iter()
            .filter(|p| !p.local)
            .cloned()
            .collect()
    }

    /// `list` as config.toml's presets: kept here too, the list on screen redrawn.
    fn write_presets(&mut self, list: Vec<Preset>) -> Vec<Action> {
        let known = self.main_presets().into_iter().map(|p| p.name).collect();
        let local: Vec<Preset> = self
            .config
            .presets
            .iter()
            .filter(|p| p.local)
            .cloned()
            .collect();
        self.config.presets = list.iter().cloned().chain(local).collect();
        let names: Vec<String> = self.config.presets.iter().map(|p| p.name.clone()).collect();
        for o in &mut self.overlays {
            if let Overlay::Presets { picker, .. } = o {
                let at = picker.highlight();
                *picker = ListPicker::new(names.clone(), |n: &String| n.clone(), false);
                picker.select_index(at.min(names.len().saturating_sub(1)));
            }
        }
        if names.is_empty() {
            self.overlays
                .retain(|o| !matches!(o, Overlay::Presets { .. }));
        }
        vec![Action::WriteConfig(ConfigEdit::Presets { list, known })]
    }

    /// A key in the name box: Enter saves or renames, asking before it replaces.
    fn preset_name_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::PresetName {
            input,
            name_for,
            replace,
        }) = self.overlays.last_mut()
        else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => {
                self.overlays.pop();
                self.message = None;
                return vec![];
            }
            KeyCode::Enter => {}
            _ => {
                input.key(key);
                *replace = false;
                return vec![];
            }
        }
        let name = input.text().trim().to_string();
        let (name_for, asked) = (name_for.clone(), *replace);
        if name.is_empty() {
            self.message = Some("a preset needs a name".into());
            return vec![];
        }
        let taken = self.config.presets.iter().find(|p| p.name == name).cloned();
        if taken.as_ref().is_some_and(|p| p.local) {
            self.message = Some(format!("{name} is set in config.local.toml"));
            return vec![];
        }
        let mut list = self.main_presets();
        match name_for {
            overlay::NameFor::Save(mut preset) => {
                if taken.is_some() && !asked {
                    if let Some(Overlay::PresetName { replace, .. }) = self.overlays.last_mut() {
                        *replace = true;
                    }
                    self.message = Some(format!(
                        "replace preset {name}? Enter: replace · Esc: keep it"
                    ));
                    return vec![];
                }
                preset.name = name.clone();
                match list.iter_mut().find(|p| p.name == name) {
                    Some(p) => *p = preset,
                    None => list.push(preset),
                }
                self.message = Some(format!("saved preset {name}"));
            }
            overlay::NameFor::Rename(old) => {
                if taken.is_some() && name != old {
                    self.message = Some(format!("there is a preset {name} already"));
                    return vec![];
                }
                if let Some(p) = list.iter_mut().find(|p| p.name == old) {
                    p.name = name;
                }
            }
        }
        self.overlays.pop();
        self.write_presets(list)
    }

    /// The new-task prompt with a preset's CLI, model and effort, and its words around
    /// the cursor.
    fn prompt_with(&mut self, preset: termist_core::config::Preset) -> Vec<Action> {
        let Some(project) = self.project else {
            self.message = Some("no project open".into());
            return vec![];
        };
        let target = self.empty_worktree().or_else(|| self.card_worktree());
        let actions = self.quick_prompt_in(project, "", target);
        if let Some(Overlay::QuickPrompt(q)) = self.overlays.last_mut() {
            let text = format!("{}{}", preset.prefix, preset.postfix);
            let mut input = TextInput::with_text_at(&text, preset.prefix.len(), true);
            input.set_history(self.prompt_history.clone());
            q.input = input;
            q.launch = LaunchOptions {
                harness: preset.harness,
                model: preset.model.clone(),
                effort: preset.effort.clone(),
            };
            q.preset = Some(preset);
        }
        actions
    }

    /// `g`: the diff of the selection's folder: a band's stand-in's worktree, else the
    /// card's worktree or folder, else the project's.
    fn open_local_diff(&mut self) -> Vec<Action> {
        let Some(path) = self.selection_folder() else {
            return vec![];
        };
        let mut actions = self.stop_scrolling();
        self.mode = Mode::Grid;
        self.view = View::Diff(crate::diff::local::LocalView::new(path.clone()));
        actions.push(Action::Send(ClientRequest::SetLocalDiff {
            path: Some(path),
            mode: DiffMode::Branch,
        }));
        actions
    }

    /// The selection's folder, for `g`, `L`, `O`, `f` and `F`: a band's stand-in's
    /// worktree, else the card's worktree or folder, else the project's.
    fn selection_folder(&self) -> Option<PathBuf> {
        let card = self
            .selected
            .and_then(|id| self.state.sessions.iter().find(|s| s.id == id))
            .map(|s| match s.place.as_deref() {
                Some(p) if !p.gone => p.root.clone(),
                _ => s.cwd.clone(),
            });
        let project = self
            .project
            .and_then(|id| self.state.projects.iter().find(|p| p.id == id))
            .map(|p| p.path.clone());
        self.empty_worktree().map(|(p, _)| p).or(card).or(project)
    }

    /// `f` and `F`: the finder on the selection's folder; `f` asks for the files now.
    fn open_finder(&mut self, kind: FindKind) -> Vec<Action> {
        let Some(folder) = self.selection_folder() else {
            return vec![];
        };
        let mut finder = Finder::new(kind, folder);
        let mut actions = vec![];
        if kind == FindKind::Files {
            actions.push(self.ask_files(&mut finder));
        }
        self.overlays.push(Overlay::Finder(finder));
        actions
    }

    fn ask_files(&mut self, finder: &mut Finder) -> Action {
        self.next_ticket += 1;
        finder.ticket = self.next_ticket;
        finder.waiting = true;
        Action::Send(ClientRequest::ListFiles {
            folder: finder.folder.clone(),
            ticket: finder.ticket,
        })
    }

    /// A key in the finder: the query, the choice, `Tab` to the other kind, `Enter` to
    /// the editor.
    fn finder_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Finder(mut f)) = self.overlays.pop() else {
            return vec![];
        };
        let mut actions = vec![];
        let last = f.hits.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return actions,
            KeyCode::Enter => {
                let Some((path, line)) = f.chosen() else {
                    self.overlays.push(Overlay::Finder(f));
                    return actions;
                };
                let root = f.root.clone().unwrap_or_else(|| f.folder.clone());
                let file = root.join(path).display().to_string();
                return self.open_editor(f.folder, Some(file), line);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                f.kind = match f.kind {
                    FindKind::Files => FindKind::Grep,
                    FindKind::Grep => FindKind::Files,
                };
                f.hits.clear();
                f.highlight = 0;
                f.failed = None;
                match (f.kind, f.files.is_some()) {
                    (FindKind::Files, false) => actions.push(self.ask_files(&mut f)),
                    _ => f.typed(now),
                }
            }
            KeyCode::Up => f.highlight = f.highlight.saturating_sub(1),
            KeyCode::Char('p') if ctrl => f.highlight = f.highlight.saturating_sub(1),
            KeyCode::Down => f.highlight = (f.highlight + 1).min(last),
            KeyCode::Char('n') if ctrl => f.highlight = (f.highlight + 1).min(last),
            KeyCode::Backspace => {
                f.query.pop();
                f.typed(now);
            }
            KeyCode::Char('u') if ctrl => {
                f.query.clear();
                f.typed(now);
            }
            KeyCode::Char(c) if !ctrl => {
                f.query.push(c);
                f.typed(now);
            }
            _ => {}
        }
        self.overlays.push(Overlay::Finder(f));
        actions
    }

    /// What is due now with no key: `F`'s ask, a moment after the last key.
    pub fn due(&mut self, now: Instant) -> Vec<Action> {
        let ticket = self.next_ticket + 1;
        let Some(Overlay::Finder(f)) = self.overlays.last_mut() else {
            return vec![];
        };
        let Some(query) = f.due_query(now) else {
            return vec![];
        };
        f.ticket = ticket;
        let folder = f.folder.clone();
        self.next_ticket = ticket;
        vec![Action::Send(ClientRequest::Grep {
            folder,
            query,
            ticket,
        })]
    }

    /// The finder that asked under `ticket`, if it is still up.
    fn finder_for(&mut self, ticket: u64) -> Option<&mut Finder> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::Finder(f) if f.ticket == ticket => Some(f),
            _ => None,
        })
    }

    /// `L`: lazygit in the selection's folder, as a card typed into at once.
    fn open_lazygit(&mut self) -> Vec<Action> {
        let (Some(project), Some(folder)) = (self.project, self.selection_folder()) else {
            return vec![];
        };
        self.focus_next_created = true;
        self.tool_from = self.selected;
        let (cols, rows) = self.pane;
        vec![Action::Send(ClientRequest::CreateSession {
            project,
            kind: SessionKind::Tool {
                program: "lazygit".into(),
                args: vec![],
            },
            cwd: Some(folder),
            issue: None,
            title_from: None,
            prompt: None,
            model: None,
            effort: None,
            cols: cols.max(20),
            rows: rows.max(5),
        })]
    }

    /// The editor the user asked for: config.toml's, else `$VISUAL`, else `$EDITOR`.
    fn preferred_editor(&self) -> Option<String> {
        self.config
            .editor
            .clone()
            .or_else(|| std::env::var("VISUAL").ok())
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.trim().is_empty())
    }

    /// `O`, and a finder's result: the selection's folder, or a file in it at a line,
    /// in the editor. A terminal one comes back as a card to type into.
    fn open_editor(
        &mut self,
        folder: PathBuf,
        file: Option<String>,
        line: Option<u32>,
    ) -> Vec<Action> {
        let Some(project) = self.project else {
            return vec![];
        };
        let editor = self.preferred_editor();
        if !termist_core::gui_editor(editor.as_deref()) {
            self.focus_next_created = true;
            self.tool_from = self.selected;
        }
        vec![Action::Send(ClientRequest::OpenInEditor {
            project,
            folder,
            file,
            line,
            editor,
        })]
    }

    /// Back to the grid from a folder's diff; the daemon stops watching the folder.
    fn close_local_diff(&mut self) -> Vec<Action> {
        self.view = View::Grid;
        vec![Action::Send(ClientRequest::SetLocalDiff {
            path: None,
            mode: DiffMode::Branch,
        })]
    }

    /// A key in a folder's diff: `u` the other mode, `R` read again, `q`/`Esc` back;
    /// the rest are the diff's own.
    fn local_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let layout = self.pr_layout.borrow().diff.clone();
        let help = self.keymap.action(Context::Grid, &key) == Some(KeyAction::Help);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let View::Diff(l) = &mut self.view else {
            return vec![];
        };
        if !l.view.typing {
            let read = |l: &crate::diff::local::LocalView| {
                vec![Action::Send(ClientRequest::SetLocalDiff {
                    path: Some(l.path.clone()),
                    mode: l.mode,
                })]
            };
            match key.code {
                KeyCode::Char('c') if ctrl => {
                    self.mode = Mode::ConfirmQuit;
                    return vec![];
                }
                KeyCode::Char('u') if !ctrl => {
                    l.flip_mode();
                    return read(l);
                }
                KeyCode::Char('R') => return read(l),
                KeyCode::Char('q') if !ctrl => return self.close_local_diff(),
                _ if help => return self.act(KeyAction::Help),
                _ => {}
            }
        }
        let files = l.diff.as_ref().map(|d| d.files.as_slice());
        match l.view.key(key, files, &layout) {
            None | Some(DiffAction::Pr(_)) => vec![],
            Some(DiffAction::Back(_)) => self.close_local_diff(),
            Some(DiffAction::Viewed { path, viewed }) => {
                vec![Action::Send(ClientRequest::SetReviewed {
                    worktree: l.path.clone(),
                    file: path,
                    reviewed: viewed,
                })]
            }
            Some(DiffAction::FlipLayout) => self.flip_layout(),
            Some(DiffAction::Note(why)) => {
                self.message = Some(why.into());
                vec![]
            }
        }
    }

    /// The wheel and a click in a folder's diff.
    fn local_mouse(&mut self, ev: MouseEvent) -> Vec<Action> {
        let layout = self.pr_layout.borrow().diff.clone();
        let View::Diff(l) = &mut self.view else {
            return vec![];
        };
        let files = l.diff.as_ref().map(|d| d.files.as_slice());
        match ev.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = ev.kind == MouseEventKind::ScrollDown;
                l.view.wheel(ev.column, ev.row, down, files, &layout);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                l.view.click(ev.column, ev.row, files, &layout);
            }
            _ => {}
        }
        vec![]
    }

    /// `s` in a diff: unified or split, kept in config.toml.
    fn flip_layout(&mut self) -> Vec<Action> {
        self.config.diff.layout = self.config.diff.layout.other();
        vec![Action::WriteConfig(ConfigEdit::Set {
            key: "diff.layout",
            value: self.config.diff.layout.id().to_string(),
        })]
    }

    /// The body shows the project's archived cards.
    pub fn archive_view(&self) -> bool {
        self.view == View::Archive
    }

    fn set_archive_view(&mut self, on: bool) {
        if on {
            // The archive is of this tab: a tab still on its way must not replace it.
            self.project_pending = None;
        }
        self.view = if on { View::Archive } else { View::Grid };
        self.selected = None;
        self.card_scroll = 0;
        self.repair_selection();
    }

    /// The archive view: move, restore (Enter), delete (d), leave (A, Esc). Keys that
    /// start or reach live sessions do nothing here.
    /// The archive view: Esc leaves and Enter restores; of the grid's keys only moving,
    /// leaving, killing and quitting work.
    fn archive_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Esc => self.set_archive_view(false),
            KeyCode::Enter => return self.restore(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = Mode::ConfirmQuit
            }
            _ => {
                if let Some(
                    action @ (KeyAction::ArchiveView
                    | KeyAction::HalfPageDown
                    | KeyAction::HalfPageUp
                    | KeyAction::Kill
                    | KeyAction::Quit
                    | KeyAction::NextTab
                    | KeyAction::PrevTab
                    | KeyAction::Left
                    | KeyAction::Down
                    | KeyAction::Up
                    | KeyAction::Right),
                ) = self.keymap.action(Context::Grid, &key)
                {
                    return self.act(action);
                }
            }
        }
        vec![]
    }

    /// Does what a grid or focus-mode key is bound to.
    fn act(&mut self, action: KeyAction) -> Vec<Action> {
        match action {
            KeyAction::Quit => self.mode = Mode::ConfirmQuit,
            KeyAction::Focus if self.selected.is_some() => return self.enter(),
            KeyAction::Focus if self.empty.is_some() => return self.start_in_empty(),
            KeyAction::Focus => {}
            KeyAction::Grid => self.mode = Mode::Grid,
            KeyAction::NewSession => return self.open_picker(),
            KeyAction::QuickPrompt => return self.open_quick_prompt(),
            KeyAction::SameTask => return self.same_task(),
            KeyAction::RemoveWorktree => {
                match self.empty_worktree().or_else(|| self.card_worktree()) {
                    Some((path, _)) => self.ask_remove(path),
                    None => {
                        self.message = Some("the project's own folder is not a worktree".into())
                    }
                }
            }
            KeyAction::Worktrees => self.open_worktrees(),
            KeyAction::LocalDiff => return self.open_local_diff(),
            KeyAction::Presets => self.open_presets(),
            KeyAction::Lazygit => return self.open_lazygit(),
            KeyAction::FindFile => return self.open_finder(FindKind::Files),
            KeyAction::Grep => return self.open_finder(FindKind::Grep),
            KeyAction::Editor => {
                if let Some(folder) = self.selection_folder() {
                    return self.open_editor(folder, None, None);
                }
            }
            KeyAction::NewShell => return self.create(SessionKind::Shell),
            KeyAction::FollowUp => self.open_follow_up(),
            KeyAction::Rename => self.open_rename(),
            KeyAction::Archive => {
                if let Some(id) = self.selected {
                    self.mode = Mode::ConfirmArchive(id);
                }
            }
            KeyAction::ArchiveView => self.set_archive_view(!self.archive_view()),
            KeyAction::PullRequests => self.toggle_prs(Section::Pulls),
            KeyAction::Issues => self.toggle_prs(Section::Issues),
            KeyAction::PullRequestInBrowser => return self.card_pr_in_browser(),
            KeyAction::RefreshGitHub => {
                if let Some(project) = self.project
                    && self.config.github.enabled
                {
                    return vec![Action::Send(ClientRequest::RefreshPrs { project })];
                }
            }
            KeyAction::Palette => self.open_palette(),
            KeyAction::HalfPageDown => self.half_page(1),
            KeyAction::HalfPageUp => self.half_page(-1),
            KeyAction::ScrollBack => {
                // `C-a [` while typing starts where you are; `PgUp` a page back.
                let lines = match self.mode {
                    Mode::Focus => 0,
                    _ => self.pane.1.max(1) as u32,
                };
                return self.start_copy(lines);
            }
            KeyAction::Kill => {
                if let Some(id) = self.selected {
                    self.mode = Mode::ConfirmKill(id);
                }
            }
            KeyAction::NextTab => self.switch_project(1),
            KeyAction::PrevTab => self.switch_project(-1),
            KeyAction::Tab(n) => {
                let tab = self
                    .open_projects()
                    .nth(n.saturating_sub(1) as usize)
                    .map(|p| p.id);
                if let Some(id) = tab {
                    self.go_to_project(id);
                }
            }
            KeyAction::OpenProject => return self.open_project_browser(),
            KeyAction::CloseTab => {
                if let Some(project) = self.project {
                    self.mode = Mode::ConfirmClose(project);
                }
            }
            KeyAction::NextAttention => return self.navigate('.'),
            KeyAction::PrevAttention => return self.navigate(','),
            KeyAction::Left => return self.navigate('h'),
            KeyAction::Down => return self.navigate('j'),
            KeyAction::Up => return self.navigate('k'),
            KeyAction::Right => return self.navigate('l'),
            KeyAction::Help => self.overlays.push(Overlay::Help { scroll: 0 }),
            KeyAction::Settings => self
                .overlays
                .push(Overlay::Settings(SettingsView::default())),
            KeyAction::TogglePane => {
                self.pane_override = Some(self.pane_position().turned(self.pane_beside));
            }
        }
        vec![]
    }

    /// Where the pane goes: `C-a z`'s choice, else the configured one.
    pub fn pane_position(&self) -> PanePosition {
        self.pane_override.unwrap_or(self.config.pane_position)
    }

    /// The colours agents are told about: the theme's, or the host terminal's.
    pub fn agent_colors(&self) -> TermColors {
        self.theme
            .agent_colors
            .or(self.host_colors)
            .unwrap_or_default()
    }

    /// Redraws in the configured theme and colour depth, and tells the daemon.
    fn apply_theme(&mut self) -> Vec<Action> {
        let depth = match self.config.colors {
            ColorDepth::Auto => self.detected_depth,
            depth => depth,
        };
        self.theme = self.themes.get(&self.config.theme, depth);
        vec![Action::Send(ClientRequest::SetColors(self.agent_colors()))]
    }

    /// Notes a change in the settings box on top: the settings or the keys list.
    fn settings_note(&mut self, note: impl Into<String>) {
        if let Some(Overlay::Settings(v) | Overlay::Keys(v)) = self.overlays.last_mut() {
            v.note = Some(note.into());
        }
    }

    fn settings_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Settings(view)) = self.overlays.last_mut() else {
            return vec![];
        };
        let row = view.row;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
            }
            KeyCode::Down | KeyCode::Char('j') => view.row = (row + 1).min(SETTING_ROWS.len() - 1),
            KeyCode::Up | KeyCode::Char('k') => view.row = row.saturating_sub(1),
            KeyCode::Left | KeyCode::Char('h') => {
                return self.change_setting(SETTING_ROWS[row], -1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                return self.change_setting(SETTING_ROWS[row], 1);
            }
            KeyCode::Enter => match SETTING_ROWS[row] {
                SettingRow::Prefix if self.local_settings.iter().any(|k| k == "prefix") => {
                    self.settings_note("prefix is set in config.local.toml");
                }
                SettingRow::Prefix => self.overlays.push(Overlay::KeyCapture(Capture {
                    target: CaptureTarget::Prefix,
                    conflict: None,
                    note: None,
                })),
                SettingRow::Keys => self.overlays.push(Overlay::Keys(SettingsView::default())),
                other => return self.change_setting(other, 1),
            },
            _ => {}
        }
        vec![]
    }

    /// ←/→ on a settings row: the next or the previous choice, saved at once.
    fn change_setting(&mut self, row: SettingRow, step: isize) -> Vec<Action> {
        let key = match row {
            SettingRow::Theme => "theme",
            SettingRow::Colors => "colors",
            SettingRow::Pane => "pane_position",
            SettingRow::DoneSound => "notify.done_sound",
            SettingRow::WaitingSound => "notify.waiting_sound",
            SettingRow::Desktop => "notify.desktop",
            SettingRow::Toasts => "notify.toasts",
            SettingRow::Splash => "scenes.splash",
            SettingRow::Idle => "scenes.idle_minutes",
            SettingRow::Animations => "animations",
            SettingRow::Mouse => "mouse",
            SettingRow::StatusCpu => "status.cpu",
            SettingRow::StatusRam => "status.ram",
            SettingRow::StatusBattery => "status.battery",
            SettingRow::StatusClock => "status.clock",
            SettingRow::PullRequests => "github.enabled",
            SettingRow::DiffLayout => "diff.layout",
            SettingRow::TeachAgents => "agents.teach",
            SettingRow::Prefix | SettingRow::Keys => return vec![],
        };
        // config.local.toml wins over the settings it sets, the old `sounds` too.
        let sound = matches!(row, SettingRow::DoneSound | SettingRow::WaitingSound);
        if let Some(set) = self
            .local_settings
            .iter()
            .find(|k| *k == key || (sound && *k == "notify.sounds"))
        {
            self.settings_note(format!("{set} is set in config.local.toml"));
            return vec![];
        }
        let next = |len: usize, at: usize| (at as isize + step).rem_euclid(len as isize) as usize;
        let quiet = |app: &mut App| {
            if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
                v.note = None;
            }
        };
        match row {
            SettingRow::DoneSound | SettingRow::WaitingSound => {
                let sound = match row {
                    SettingRow::DoneSound => &mut self.config.notify.done_sound,
                    _ => &mut self.config.notify.waiting_sound,
                };
                let all = Sound::ALL;
                let at = all.iter().position(|x| x == sound).unwrap_or(0);
                *sound = all[next(all.len(), at)];
                let chosen = *sound;
                quiet(self);
                let value = chosen.id().to_string();
                return vec![
                    Action::WriteConfig(ConfigEdit::Set { key, value }),
                    Action::Preview(chosen),
                ];
            }
            SettingRow::DiffLayout => {
                self.config.diff.layout = self.config.diff.layout.other();
                quiet(self);
                let value = self.config.diff.layout.id().to_string();
                return vec![Action::WriteConfig(ConfigEdit::Set { key, value })];
            }
            SettingRow::PullRequests => {
                self.config.github.enabled = !self.config.github.enabled;
                let value = self.config.github.enabled;
                quiet(self);
                return vec![
                    Action::WriteConfig(ConfigEdit::SetBool { key, value }),
                    Action::Send(ClientRequest::SetGitHub { enabled: value }),
                ];
            }
            SettingRow::Desktop
            | SettingRow::Toasts
            | SettingRow::Splash
            | SettingRow::Animations
            | SettingRow::Mouse
            | SettingRow::StatusCpu
            | SettingRow::StatusRam
            | SettingRow::StatusBattery
            | SettingRow::StatusClock
            | SettingRow::TeachAgents => {
                let flag = match row {
                    SettingRow::Desktop => &mut self.config.notify.desktop,
                    SettingRow::Toasts => &mut self.config.notify.toasts,
                    SettingRow::Splash => &mut self.config.scenes.splash,
                    SettingRow::Mouse => &mut self.config.mouse,
                    SettingRow::StatusCpu => &mut self.config.status.cpu,
                    SettingRow::StatusRam => &mut self.config.status.ram,
                    SettingRow::StatusBattery => &mut self.config.status.battery,
                    SettingRow::StatusClock => &mut self.config.status.clock,
                    SettingRow::TeachAgents => &mut self.config.agents.teach,
                    _ => &mut self.config.animations,
                };
                *flag = !*flag;
                let value = *flag;
                quiet(self);
                return vec![Action::WriteConfig(ConfigEdit::SetBool { key, value })];
            }
            SettingRow::Idle => {
                let at = IDLE_CHOICES
                    .iter()
                    .position(|m| *m == self.config.scenes.idle_minutes)
                    .unwrap_or(2);
                self.config.scenes.idle_minutes = IDLE_CHOICES[next(IDLE_CHOICES.len(), at)];
                quiet(self);
                let value = self.config.scenes.idle_minutes as i64;
                return vec![Action::WriteConfig(ConfigEdit::SetInt { key, value })];
            }
            _ => {}
        }
        let value = match row {
            SettingRow::Theme => {
                let ids: Vec<String> = self.themes.ids().map(String::from).collect();
                let at = ids
                    .iter()
                    .position(|t| *t == self.config.theme)
                    .unwrap_or(0);
                self.config.theme = ids[next(ids.len(), at)].clone();
                self.config.theme.clone()
            }
            SettingRow::Pane => {
                let all = PanePosition::ALL;
                let at = all
                    .iter()
                    .position(|p| *p == self.config.pane_position)
                    .unwrap_or(0);
                self.config.pane_position = all[next(all.len(), at)];
                self.pane_override = None;
                if let Some(Overlay::Settings(v)) = self.overlays.last_mut() {
                    v.note = None;
                }
                let value = self.config.pane_position.id().to_string();
                return vec![Action::WriteConfig(ConfigEdit::Set { key, value })];
            }
            _ => {
                let all = ColorDepth::ALL;
                let at = all
                    .iter()
                    .position(|d| *d == self.config.colors)
                    .unwrap_or(0);
                self.config.colors = all[next(all.len(), at)];
                self.config.colors.id().to_string()
            }
        };
        let mut actions = self.apply_theme();
        let running_agents = self.state.sessions.iter().any(|s| {
            matches!(s.kind, SessionKind::Agent { .. }) && s.status.is_live() && !s.archived
        });
        if let Some(wanted) = self.theme.stands_in_for.clone() {
            self.settings_note(format!("{wanted} needs 256 colours; this terminal has 16"));
        } else if running_agents {
            self.settings_note("running agents keep the colours they started with");
        } else if let Some(Overlay::Settings(v)) = self.overlays.last_mut() {
            v.note = None;
        }
        actions.push(Action::WriteConfig(ConfigEdit::Set { key, value }));
        actions
    }

    fn keys_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let rows = key_rows();
        let Some(Overlay::Keys(view)) = self.overlays.last_mut() else {
            return vec![];
        };
        let row = view.row.min(rows.len() - 1);
        let (context, action) = rows[row];
        let local = self.local_settings.iter().any(|k| k == "keys");
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlays.pop();
            }
            KeyCode::Down | KeyCode::Char('j') => view.row = (row + 1).min(rows.len() - 1),
            KeyCode::Up | KeyCode::Char('k') => view.row = row.saturating_sub(1),
            KeyCode::PageDown => view.row = (row + 10).min(rows.len() - 1),
            KeyCode::PageUp => view.row = row.saturating_sub(10),
            KeyCode::Enter | KeyCode::Backspace | KeyCode::Char('R') if local => {
                self.settings_note("keys are set in config.local.toml");
            }
            KeyCode::Enter => self.overlays.push(Overlay::KeyCapture(Capture {
                target: CaptureTarget::Key(context, action),
                conflict: None,
                note: None,
            })),
            KeyCode::Backspace => {
                self.keymap.set_keys(context, action, &[]);
                self.settings_note(format!("{} has no key now", action.label()));
                return vec![self.write_keys(context)];
            }
            KeyCode::Char('R') => {
                let defaults = Keymap::defaults().keys(context, action);
                self.keymap.set_keys(context, action, &defaults);
                self.settings_note(format!("{} is back on its default key", action.label()));
                return vec![self.write_keys(context)];
            }
            _ => {}
        }
        vec![]
    }

    fn write_keys(&self, context: Context) -> Action {
        Action::WriteConfig(ConfigEdit::Keys {
            table: match context {
                Context::Grid => "grid",
                Context::Focus => "focus",
            },
            bindings: self.keymap.overrides(context),
        })
    }

    /// The key pressed while capturing becomes the prefix or the action's key; a key
    /// another action has asks first.
    fn capture_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::KeyCapture(capture)) = self.overlays.last_mut() else {
            return vec![];
        };
        if key.code == KeyCode::Esc {
            self.overlays.pop();
            return vec![];
        }
        let spec = KeySpec::of(&key);
        if !Keymap::writable(&spec) {
            capture.note = Some("that key has no name config.toml can hold; try another".into());
            capture.conflict = None;
            return vec![];
        }
        match (capture.target, capture.conflict.take()) {
            (CaptureTarget::Key(context, action), Some((taken, other)))
                if key.code == KeyCode::Enter =>
            {
                // The other action gets every key this one had.
                let old: Vec<KeySpec> = self
                    .keymap
                    .keys(context, action)
                    .into_iter()
                    .filter(|k| *k != taken)
                    .collect();
                let mut others: Vec<KeySpec> = self
                    .keymap
                    .keys(context, other)
                    .into_iter()
                    .filter(|k| *k != taken)
                    .collect();
                others.extend(&old);
                self.keymap.set_keys(context, action, &[taken]);
                self.keymap.set_keys(context, other, &others);
                self.overlays.pop();
                let note = if others.is_empty() {
                    format!(
                        "{taken}: {} · {} has no key now",
                        action.label(),
                        other.label()
                    )
                } else {
                    let keys: Vec<String> = others.iter().map(|k| k.to_string()).collect();
                    format!(
                        "{taken}: {} · {}: {}",
                        action.label(),
                        keys.join(" "),
                        other.label()
                    )
                };
                self.settings_note(note);
                vec![self.write_keys(context)]
            }
            (CaptureTarget::Prefix, _) => {
                if let Some(why) = Keymap::refuses_prefix(&spec) {
                    capture.note = Some(why);
                    return vec![];
                }
                self.keymap.prefix = spec;
                // The new prefix cannot also be a key after the prefix.
                let taken = self.keymap.action(Context::Focus, &key);
                if let Some(action) = taken {
                    let keys: Vec<KeySpec> = self
                        .keymap
                        .keys(Context::Focus, action)
                        .into_iter()
                        .filter(|k| *k != spec)
                        .collect();
                    self.keymap.set_keys(Context::Focus, action, &keys);
                }
                self.config.prefix = spec.to_string();
                self.overlays.pop();
                self.settings_note(format!("the prefix is {spec}"));
                let mut actions = vec![Action::WriteConfig(ConfigEdit::Set {
                    key: "prefix",
                    value: spec.to_string(),
                })];
                // Keys that config.local.toml sets are not copied into config.toml.
                if taken.is_some() && !self.local_settings.iter().any(|k| k == "keys") {
                    actions.push(self.write_keys(Context::Focus));
                }
                actions
            }
            (CaptureTarget::Key(context, action), _) => {
                if let Some(why) = self.keymap.refuses(context, &spec) {
                    capture.note = Some(why);
                    return vec![];
                }
                match self.keymap.action(context, &key) {
                    Some(same) if same == action => {
                        self.overlays.pop();
                        vec![]
                    }
                    Some(other) => {
                        capture.note = Some(format!(
                            "{spec} is \"{}\" · Enter: swap their keys · Esc: cancel",
                            other.label()
                        ));
                        capture.conflict = Some((spec, other));
                        vec![]
                    }
                    None => {
                        self.keymap.set_keys(context, action, &[spec]);
                        self.overlays.pop();
                        self.settings_note(format!("{spec}: {}", action.label()));
                        vec![self.write_keys(context)]
                    }
                }
            }
        }
    }

    /// The help: j/k and the arrows scroll (the view stops at its last line), Esc, `?`
    /// and `q` close it.
    fn help_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Help { scroll }) = self.overlays.last_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('?' | 'q') => {
                self.overlays.pop();
            }
            KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(self.help_end.get()),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                *scroll = (*scroll + 10).min(self.help_end.get())
            }
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            _ => {}
        }
    }

    /// Enter in the archive view: the card comes back to the grid and, if it is not
    /// running, resumes; it is focused once it is back.
    fn restore(&mut self) -> Vec<Action> {
        let Some(id) = self.selected else {
            return vec![];
        };
        self.view = View::Grid;
        let mut actions = vec![Action::Send(ClientRequest::UnarchiveSession {
            session: id,
        })];
        actions.extend(self.enter());
        actions
    }

    /// Each frame's layout: cards per row and the lines they may take. Scrolls just
    /// enough to keep the selected card on screen.
    pub fn set_card_window(&mut self, per_row: usize, lines: u16) {
        self.cards_per_row = per_row.max(1);
        self.card_lines = lines.max(1);
        let rows = self.grid_rows();
        let span = |rows: &[crate::bands::Row]| rows.iter().map(|r| r.height()).sum::<u16>();
        if let Some(r) = self.selected_row(&rows) {
            self.card_scroll = self.card_scroll.min(r);
            while self.card_scroll < r && span(&rows[self.card_scroll..=r]) > self.card_lines {
                self.card_scroll += 1;
            }
        }
        // No room left empty below the last row.
        let mut first = rows.len();
        while first > 0 && span(&rows[first - 1..]) <= self.card_lines {
            first -= 1;
        }
        self.card_scroll = self.card_scroll.min(first);
        self.card_rows = (self.card_scroll..rows.len())
            .take_while(|&i| span(&rows[self.card_scroll..=i]) <= self.card_lines)
            .count()
            .max(1);
    }

    /// The rows of the grid as drawn now: the cards by band, `cards_per_row` to a row.
    pub fn grid_rows(&self) -> Vec<crate::bands::Row> {
        let sessions = self.project_sessions();
        let (bands, headers) = self.grid_bands(&sessions);
        crate::bands::rows(&bands, self.cards_per_row, headers)
    }

    /// The bands of `sessions` and whether they get headers; the archive is one band.
    pub fn grid_bands<'a>(
        &'a self,
        sessions: &[&'a SessionInfo],
    ) -> (Vec<crate::bands::Band<'a>>, bool) {
        let path = self
            .state
            .projects
            .iter()
            .find(|p| Some(p.id) == self.project)
            .map(|p| p.path.clone())
            .unwrap_or_default();
        if self.archive_view() {
            let all = crate::bands::Band {
                root: path,
                place: None,
                cards: sessions.to_vec(),
                worktree: None,
            };
            return (vec![all], false);
        }
        let bands = crate::bands::bands(&path, sessions, self.project_worktrees());
        let headers = crate::bands::headers(&path, &bands);
        (bands, headers)
    }

    /// What the cards take, for the layout.
    pub fn card_shape(&self) -> crate::ui::Shape {
        let sessions = self.project_sessions();
        let (bands, headers) = self.grid_bands(&sessions);
        crate::ui::Shape {
            // A band with no cards has its stand-in.
            counts: bands.iter().map(|b| b.cards.len().max(1)).collect(),
            headers,
        }
    }

    fn selected_row(&self, rows: &[crate::bands::Row]) -> Option<usize> {
        let here = self.slot()?;
        rows.iter().position(|r| r.slots.contains(&here))
    }

    /// `delta` rows down (up when negative), in the same column as far as the row goes;
    /// past the last row (or the first) to the last card (or the first).
    fn move_rows(&mut self, delta: isize) {
        let rows = self.grid_rows();
        if rows.is_empty() {
            return;
        }
        let here = self.slot();
        let (r, col) = match self.selected_row(&rows) {
            Some(r) => (
                r,
                rows[r].slots.iter().position(|s| Some(s) == here.as_ref()),
            ),
            None => (0, Some(0)),
        };
        let to = r as isize + delta;
        let last = rows.len() as isize - 1;
        let slot = match to {
            ..0 => rows[0].slots[0].clone(),
            _ if to > last => rows[last as usize]
                .slots
                .last()
                .expect("rows have slots")
                .clone(),
            _ => {
                let row = &rows[to as usize].slots;
                row[col.unwrap_or(0).min(row.len() - 1)].clone()
            }
        };
        self.set_slot(slot);
    }

    /// Ctrl+D / Ctrl+U: half a screen of cards down or up, selection and view together.
    fn half_page(&mut self, direction: isize) {
        let half = (self.card_rows / 2).max(1);
        self.move_rows(direction * half as isize);
        self.card_scroll = self
            .card_scroll
            .saturating_add_signed(direction * half as isize);
        let (per_row, lines) = (self.cards_per_row, self.card_lines);
        self.set_card_window(per_row, lines);
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
        self.go_to_project(open[((pos + delta).rem_euclid(n)) as usize]);
    }

    /// The user shows the tab of an open project, on its first card; a tab still on its
    /// way no longer comes forward.
    fn go_to_project(&mut self, id: ProjectId) {
        self.project_pending = None;
        self.project = Some(id);
        self.selected = None;
        self.repair_selection();
    }

    /// After `OpenProject` or `AddProject`, the state that has the project open brings
    /// its tab to the front.
    fn switch_to_pending_project(&mut self) {
        let found = match &self.project_pending {
            Some(ProjectPending::Known { project, .. }) => self
                .state
                .projects
                .iter()
                .find(|p| p.id == *project && p.open),
            Some(ProjectPending::Path(path)) => self.open_projects().find(|p| p.path == *path),
            None => None,
        };
        if let Some(id) = found.map(|p| p.id) {
            let select = match self.project_pending.take() {
                Some(ProjectPending::Known { select, .. }) => select,
                _ => None,
            };
            self.project = Some(id);
            // `repair_selection` falls back to the first card if this one is gone.
            self.selected = select;
        }
    }

    /// A card the user started or resumed is here: it takes the focus from the grid or
    /// the pane. Under a question it is only selected, so the question keeps its keys;
    /// the archive view keeps its own selection.
    fn arrived(&mut self, id: SessionId) {
        match self.mode {
            Mode::Grid if self.archive_view() => {}
            Mode::Grid | Mode::Focus => {
                self.select(id);
                self.view = View::Grid;
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
        // A band's stand-in stays selected while its worktree is shown with no cards.
        let empty = self.selected.is_none()
            && self.empty.as_ref().is_some_and(|path| {
                self.project_worktrees()
                    .iter()
                    .any(|w| &w.path == path && w.shown)
                    && !self
                        .project_sessions()
                        .iter()
                        .any(|s| s.place.as_deref().map_or(&s.cwd, |p| &p.root) == path)
            });
        if !empty {
            self.empty = None;
        }
        if !valid && !empty {
            self.selected = self.project_sessions().first().map(|s| s.id);
            if matches!(self.mode, Mode::Focus | Mode::FocusPrefix) {
                self.mode = Mode::Grid;
            }
            // No card: the first band's stand-in, if a worktree is shown.
            if self.selected.is_none()
                && let Some(Slot::Empty(path)) =
                    self.grid_rows().into_iter().flat_map(|r| r.slots).next()
            {
                self.empty = Some(path);
            }
        }
    }

    fn sync_attachment(&mut self) -> Vec<Action> {
        let (cols, rows) = self.pane;
        if self.selected == self.attached || cols == 0 || rows == 0 {
            return vec![];
        }
        let mut actions = Vec::new();
        // The daemon opens a card on its live screen.
        self.scrolling = false;
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

/// A folder's last part, as messages name a worktree.
fn short(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
            cwd: "/p".into(),
            place: None,
            issue: None,
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
        app.set_card_window(2, 4 * crate::ui::CARD_H);
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
                cwd: None,
                issue: None,
                title_from: None,
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

    fn preset(name: &str, prefix: &str, postfix: &str) -> termist_core::config::Preset {
        termist_core::config::Preset {
            name: name.into(),
            harness: Harness::Codex,
            model: Some("gpt-5".into()),
            effort: Some("high".into()),
            prefix: prefix.into(),
            postfix: postfix.into(),
            local: false,
        }
    }

    fn with_presets(app: &mut App) {
        app.on_event(ServerEvent::Harnesses(vec![
            HarnessInfo {
                harness: Harness::Claude,
                available: true,
            },
            HarnessInfo {
                harness: Harness::Codex,
                available: true,
            },
        ]));
        app.config.presets = vec![
            preset("review", "Review this: ", "\nThen list what to fix."),
            preset("plain", "", ""),
        ];
    }

    #[test]
    fn e_lists_the_presets_and_enter_opens_the_prompt_ready_for_the_task() {
        let (mut app, _) = app();
        with_presets(&mut app);
        app.on_key(k(K::Char('e')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Presets { .. })));
        app.on_key(k(K::Enter));
        let q = quick_prompt(&app);
        assert_eq!(
            (
                q.launch.harness,
                q.launch.model.as_deref(),
                q.launch.effort.as_deref()
            ),
            (Harness::Codex, Some("gpt-5"), Some("high"))
        );
        assert_eq!(q.input.text(), "Review this: \nThen list what to fix.");
        assert_eq!(
            q.input.split_at_cursor().0,
            "Review this: ",
            "typed between its words"
        );
        for c in "fix login".chars() {
            app.on_key(k(K::Char(c)));
        }
        let actions = app.on_key(k(K::Enter));
        assert!(matches!(
            sent(&actions)[..],
            [_, ClientRequest::CreateSession { prompt: Some(prompt), title_from: Some(from), .. }]
                if prompt == "Review this: fix login\nThen list what to fix." && from == "fix login"
        ));
    }

    #[test]
    fn a_preset_s_words_edited_at_the_end_still_name_the_card_after_what_is_typed() {
        let (mut app, _) = app();
        with_presets(&mut app);
        app.on_key(k(K::Char('e')));
        app.on_key(k(K::Enter));
        for c in "fix login".chars() {
            app.on_key(k(K::Char(c)));
        }
        // The postfix's line break deleted: the words typed are still the name.
        app.on_key(k(K::Delete));
        let actions = app.on_key(k(K::Enter));
        assert!(matches!(
            sent(&actions)[..],
            [_, ClientRequest::CreateSession { title_from: Some(from), .. }]
                if from.starts_with("fix login")
        ));
    }

    fn written(actions: &[Action]) -> Option<Vec<String>> {
        actions.iter().find_map(|a| match a {
            Action::WriteConfig(ConfigEdit::Presets { list, .. }) => {
                Some(list.iter().map(|p| p.name.clone()).collect())
            }
            _ => None,
        })
    }

    #[test]
    fn ctrl_s_saves_the_prompt_as_a_preset_split_at_the_cursor() {
        let (mut app, _) = app();
        with_presets(&mut app);
        app.on_key(k(K::Char('e')));
        app.on_key(k(K::Enter));
        for c in "the parser ".chars() {
            app.on_key(k(K::Char(c)));
        }
        app.on_key(ctrl('s'));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::PresetName { .. })
        ));
        app.on_key(ctrl('u'));
        for c in "parser".chars() {
            app.on_key(k(K::Char(c)));
        }
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            written(&actions),
            Some(vec!["review".into(), "plain".into(), "parser".into()])
        );
        let saved = app
            .config
            .presets
            .iter()
            .find(|p| p.name == "parser")
            .unwrap();
        assert_eq!(
            saved.prefix, "Review this: the parser ",
            "before the cursor"
        );
        assert_eq!(saved.postfix, "\nThen list what to fix.", "after it");
        assert_eq!(
            (saved.harness, saved.model.as_deref()),
            (Harness::Codex, Some("gpt-5"))
        );
        assert!(
            matches!(app.overlays.last(), Some(Overlay::QuickPrompt(_))),
            "the task is still to start"
        );
        assert_eq!(app.message.as_deref(), Some("saved preset parser"));
        // The same name again asks first.
        app.on_key(ctrl('s'));
        app.on_key(ctrl('u'));
        for c in "review".chars() {
            app.on_key(k(K::Char(c)));
        }
        assert_eq!(written(&app.on_key(k(K::Enter))), None);
        assert_eq!(
            app.message.as_deref(),
            Some("replace preset review? Enter: replace · Esc: keep it")
        );
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            written(&actions),
            Some(vec!["review".into(), "plain".into(), "parser".into()])
        );
        assert_eq!(app.config.presets[0].prefix, "Review this: the parser ");
    }

    #[test]
    fn r_renames_and_d_deletes_a_preset_but_not_one_from_the_local_file() {
        let (mut app, _) = app();
        with_presets(&mut app);
        app.on_key(k(K::Char('e')));
        app.on_key(k(K::Char('r')));
        app.on_key(ctrl('u'));
        for c in "audit".chars() {
            app.on_key(k(K::Char(c)));
        }
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            written(&actions),
            Some(vec!["audit".into(), "plain".into()])
        );
        assert!(
            matches!(app.overlays.last(), Some(Overlay::Presets { .. })),
            "back in the list"
        );
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Char('d')));
        assert_eq!(app.message.as_deref(), Some("delete preset plain? y/N"));
        assert!(written(&app.on_key(k(K::Char('n')))).is_none(), "kept");
        app.on_key(k(K::Char('d')));
        let actions = app.on_key(k(K::Char('y')));
        assert_eq!(written(&actions), Some(vec!["audit".into()]));
        let mut local = preset("mine", "", "");
        local.local = true;
        app.config.presets.push(local);
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('e')));
        app.on_key(k(K::Char('j')));
        assert!(written(&app.on_key(k(K::Char('d')))).is_none());
        assert_eq!(
            app.message.as_deref(),
            Some("mine is set in config.local.toml")
        );
    }

    #[test]
    fn a_preset_with_nothing_typed_names_the_card_and_none_says_how_to_make_one() {
        let (mut app, _) = app();
        with_presets(&mut app);
        app.on_key(k(K::Char('e')));
        app.on_key(k(K::Enter));
        let actions = app.on_key(k(K::Enter));
        assert!(matches!(
            sent(&actions)[..],
            [_, ClientRequest::CreateSession { title_from: Some(from), .. }] if from == "review"
        ));
        // Without a preset the prompt names it.
        app.on_key(k(K::Char('p')));
        for c in "add tests".chars() {
            app.on_key(k(K::Char(c)));
        }
        let actions = app.on_key(k(K::Enter));
        assert!(matches!(
            sent(&actions)[..],
            [
                _,
                ClientRequest::CreateSession {
                    title_from: None,
                    ..
                }
            ]
        ));
        app.config.presets.clear();
        app.on_key(k(K::Char('e')));
        assert!(app.overlays.is_empty());
        assert_eq!(
            app.message.as_deref(),
            Some("no presets · in the new-task prompt, ^S saves one")
        );
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
                cwd: None,
                issue: None,
                title_from: None,
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
        assert!(app.archive_view());
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
                    cwd: None,
                    issue: None,
                    title_from: None,
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
                k(K::Down),
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
    fn the_catalog_fills_the_model_list() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        app.on_key(ctrl('o'));
        app.on_event(ServerEvent::Models {
            harness: Harness::Claude,
            recent: vec![],
            catalog: ["opus", "sonnet"]
                .map(|id| ModelInfo {
                    id: id.into(),
                    label: id.into(),
                    efforts: vec![],
                })
                .to_vec(),
        });
        for c in "son".chars() {
            app.on_key(k(K::Char(c)));
        }
        app.on_key(k(K::Down));
        app.on_key(k(K::Enter));
        assert_eq!(quick_prompt(&app).launch.model.as_deref(), Some("sonnet"));
    }

    // The daemon's answer must not drop the model in use, nor move the highlight off it.
    #[test]
    fn the_current_model_stays_listed_and_highlighted_when_the_list_comes() {
        let (mut app, _) = app();
        app.state.last_launch = Some(launch(Harness::Codex, Some("my-model"), None));
        app.on_key(k(K::Char('p')));
        app.on_key(ctrl('o'));
        app.on_event(ServerEvent::Models {
            harness: Harness::Codex,
            recent: vec![],
            catalog: ["gpt-a", "gpt-b"]
                .map(|id| ModelInfo {
                    id: id.into(),
                    label: id.into(),
                    efforts: vec![],
                })
                .to_vec(),
        });
        let Some(Overlay::Model(m)) = app.overlays.last() else {
            panic!("{:?}", app.overlays.last())
        };
        assert!(
            m.models
                .items()
                .iter()
                .any(|c| c.model().as_deref() == Some("my-model")),
            "still listed"
        );
        assert_eq!(
            m.models.selected().and_then(|c| c.model()).as_deref(),
            Some("my-model"),
            "still highlighted"
        );
        app.on_key(k(K::Enter));
        assert_eq!(quick_prompt(&app).launch.model.as_deref(), Some("my-model"));
    }

    // The daemon keeps the launch line but does not send the state again; the next
    // quick prompt must not open on the old one (and send that back).
    #[test]
    fn the_next_quick_prompt_opens_on_the_launch_just_used() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        app.on_key(k(K::Tab));
        app.on_key(k(K::Char('2')));
        app.on_key(ctrl('o'));
        app.on_event(ServerEvent::Models {
            harness: Harness::Codex,
            recent: vec!["gpt-5".into()],
            catalog: vec![],
        });
        for key in [K::Down, K::Right, K::Right, K::Enter] {
            app.on_key(k(key));
        }
        let used = quick_prompt(&app).launch.clone();
        assert_eq!(used.harness, Harness::Codex);
        assert_eq!(used.model.as_deref(), Some("gpt-5"));
        assert!(used.effort.is_some());
        app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        app.on_key(k(K::Char('p')));
        assert_eq!(quick_prompt(&app).launch, used);
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
            catalog: vec![],
        });
        for key in [K::Down, K::Down, K::Right, K::Right, K::Right, K::Enter] {
            app.on_key(k(key));
        }
        assert_eq!(
            quick_prompt(&app).launch,
            launch(Harness::Claude, Some("sonnet"), Some("high"))
        );
        app.on_key(ctrl('o'));
        for key in [K::Up, K::Up, K::Left, K::Left, K::Left, K::Enter] {
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
        app.on_key(k(K::Right));
        app.on_key(k(K::Down));
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
        assert!(
            quick_prompt(&app).input.is_empty(),
            "a recalled prompt left as it was is not a draft"
        );
        app.on_key(k(K::Up));
        assert_eq!(
            quick_prompt(&app).input.text(),
            "the last one",
            "remembered for the next prompt"
        );
        app.on_key(k(K::Up));
        assert_eq!(quick_prompt(&app).input.text(), "older");
    }

    #[test]
    fn a_recalled_prompt_that_was_edited_is_a_draft() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        app.on_event(ServerEvent::PromptHistory(vec!["the last one".into()]));
        app.on_key(k(K::Up));
        type_text(&mut app, " again");
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('p')));
        assert_eq!(quick_prompt(&app).input.text(), "the last one again");
    }

    // Esc is often a slip: the text waits for the next `p`, cursor at its end.
    #[test]
    fn esc_keeps_the_quick_prompt_text_for_the_next_p() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        type_text(&mut app, "fix the bug");
        app.on_key(KeyEvent::new(K::Enter, M::SHIFT));
        type_text(&mut app, "and test it");
        app.on_key(k(K::Esc));
        assert!(app.overlays.is_empty());
        app.on_key(k(K::Char('p')));
        let input = &quick_prompt(&app).input;
        assert_eq!(input.text(), "fix the bug\nand test it");
        assert_eq!(input.cursor_line_col(), (1, 11), "the cursor at the end");
        app.on_event(ServerEvent::PromptHistory(vec!["older".into()]));
        app.on_key(k(K::Up));
        assert_eq!(
            quick_prompt(&app).input.text(),
            "fix the bug\nand test it",
            "the history never replaces the draft"
        );

        // Ctrl+Q keeps the text as it is then (↑ moved to the first line), and `C-a p`
        // opens on it too
        type_text(&mut app, " now");
        app.on_key(ctrl('q'));
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('p')));
        assert_eq!(
            quick_prompt(&app).input.text(),
            "fix the bug now\nand test it"
        );
    }

    #[test]
    fn a_started_or_blank_quick_prompt_leaves_no_draft() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('p')));
        type_text(&mut app, "  ");
        app.on_key(KeyEvent::new(K::Enter, M::SHIFT));
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('p')));
        assert!(quick_prompt(&app).input.is_empty(), "only blanks");
        type_text(&mut app, "ship it");
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('p')));
        app.on_key(k(K::Enter));
        assert!(app.overlays.is_empty());
        app.on_key(k(K::Char('p')));
        assert!(quick_prompt(&app).input.is_empty(), "started, so gone");
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
    fn a_follow_up_can_be_typed_over_several_lines() {
        let (mut app, s) = app();
        let mut screen = Snapshot::blank(10, 2);
        screen.modes.bracketed_paste = true;
        app.screens.insert(s[0].id, screen);
        app.on_key(k(K::Char(' ')));
        type_text(&mut app, "first");
        app.on_key(KeyEvent::new(K::Enter, M::ALT));
        type_text(&mut app, "second");
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions)[0],
            &ClientRequest::Input {
                session: s[0].id,
                data: b"\x1b[200~first\nsecond\x1b[201~".to_vec()
            }
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

    /// The attached card's screen as the daemon last sent it: `offset` lines back out
    /// of `history`.
    fn history(app: &mut App, offset: u32, history: u32) -> SessionId {
        let id = app.attached.unwrap();
        let mut screen = Snapshot::blank(80, 20);
        screen.scroll = termist_core::ScrollPos { offset, history };
        app.screens.insert(id, screen);
        id
    }

    fn scrolls(actions: &[Action]) -> Vec<Scroll> {
        sent(actions)
            .into_iter()
            .filter_map(|r| match r {
                ClientRequest::Scroll { scroll, .. } => Some(*scroll),
                _ => None,
            })
            .collect()
    }

    fn wheel(app: &mut App, up: bool, column: u16, row: u16) -> Vec<Action> {
        app.on_mouse(MouseEvent {
            kind: if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column,
            row,
            modifiers: M::NONE,
        })
    }

    #[test]
    fn c_a_bracket_opens_copy_mode_whose_keys_never_reach_the_session() {
        let (mut app, _) = app();
        history(&mut app, 0, 500);
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        app.on_key(ctrl('a'));
        let actions = app.on_key(k(K::Char('[')));
        assert!(app.scrolling && app.copy.is_some());
        assert!(sent(&actions).is_empty(), "the cursor starts on the screen");
        assert_eq!(
            app.copy.as_ref().unwrap().cursor.line,
            519,
            "500 back, 20 on screen"
        );
        for _ in 0..19 {
            assert!(sent(&app.on_key(k(K::Char('k')))).is_empty());
        }
        assert_eq!(
            scrolls(&app.on_key(k(K::Char('k')))),
            vec![Scroll::Top, Scroll::Lines(-499)],
            "off the screen: the view goes along"
        );
        assert!(sent(&app.on_key(k(K::Char('x')))).is_empty(), "not typed");
        assert_eq!(scrolls(&app.on_key(k(K::Char('q')))), vec![Scroll::Bottom]);
        assert!(!app.scrolling && app.copy.is_none());
        assert_eq!(app.mode, Mode::Focus, "back to typing into the session");
    }

    #[test]
    fn page_up_opens_copy_mode_a_page_back_even_without_history() {
        let (mut app, _) = app();
        history(&mut app, 0, 500);
        let actions = app.on_key(k(K::PageUp));
        assert!(app.copy.is_some());
        assert_eq!(scrolls(&actions), vec![Scroll::Top, Scroll::Lines(-480)]);
        app.on_key(k(K::Char('q')));
        history(&mut app, 0, 0);
        assert!(sent(&app.on_key(k(K::PageUp))).is_empty());
        assert!(app.copy.is_some(), "the screen alone can be copied from");
        assert_eq!(
            app.copy.as_ref().unwrap().cursor.line,
            0,
            "a page back: the top"
        );
    }

    #[test]
    fn y_copies_what_the_daemon_cuts_and_ends_copy_mode() {
        let (mut app, _) = app();
        let id = history(&mut app, 0, 500);
        app.on_key(k(K::PageUp));
        history(&mut app, 20, 500);
        app.on_key(k(K::Char('V')));
        app.on_key(k(K::Char('k')));
        let actions = app.on_key(k(K::Char('y')));
        let at = |line| termist_core::Pos { line, col: 0 };
        assert!(sent(&actions).contains(&&ClientRequest::CopyText {
            session: id,
            from: at(499),
            to: at(498),
            lines: true,
        }));
        assert_eq!(
            scrolls(&actions),
            vec![Scroll::Bottom],
            "back to the live screen"
        );
        assert!(app.copy.is_none());
        let actions = app.on_event(ServerEvent::CopiedText {
            session: id,
            text: "a\nb".into(),
        });
        assert_eq!(actions, vec![Action::Copy("a\nb".into())]);
        assert!(app.toasts.items().any(|t| t.text == "✓ copied 2 lines"));
        assert!(
            app.on_event(ServerEvent::CopiedText {
                session: id,
                text: "again".into(),
            })
            .is_empty(),
            "only what was asked for"
        );
    }

    #[test]
    fn a_search_in_copy_mode_goes_through_the_daemon_and_moves_the_view() {
        let (mut app, _) = app();
        let id = history(&mut app, 0, 500);
        app.on_key(ctrl('a'));
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('[')));
        app.on_key(k(K::Char('?')));
        for c in "error".chars() {
            app.on_key(k(K::Char(c)));
        }
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::Search {
                session: id,
                query: "error".into(),
                from: termist_core::Pos { line: 519, col: 0 },
                backward: true,
            }]
        );
        let at = termist_core::Pos { line: 100, col: 4 };
        let actions = app.on_event(ServerEvent::Found {
            session: id,
            at: Some((at, at)),
            index: 3,
            total: 9,
        });
        assert_eq!(scrolls(&actions), vec![Scroll::Top, Scroll::Lines(-100)]);
        assert_eq!(app.copy.as_ref().unwrap().cursor, at);
    }

    #[test]
    fn the_wheel_over_the_pane_scrolls_the_history() {
        let (mut app, _) = app();
        app.pane_area = Rect::new(0, 10, 80, 20);
        history(&mut app, 0, 500);
        assert!(
            sent(&wheel(&mut app, true, 5, 3)).is_empty(),
            "over the cards"
        );
        assert_eq!(
            scrolls(&wheel(&mut app, true, 5, 12)),
            vec![Scroll::Lines(3)]
        );
        assert!(app.scrolling);
        history(&mut app, 6, 500);
        assert_eq!(
            scrolls(&wheel(&mut app, true, 5, 12)),
            vec![Scroll::Lines(3)]
        );
        assert_eq!(
            scrolls(&wheel(&mut app, false, 5, 12)),
            vec![Scroll::Lines(-3)]
        );
        history(&mut app, 3, 500);
        assert_eq!(
            scrolls(&wheel(&mut app, false, 5, 12)),
            vec![Scroll::Bottom]
        );
        assert!(!app.scrolling);
        assert!(
            sent(&wheel(&mut app, false, 5, 12)).is_empty(),
            "already live"
        );
    }

    #[test]
    fn a_program_that_wants_the_mouse_gets_the_wheel() {
        let (mut app, _) = app();
        app.pane_area = Rect::new(0, 10, 80, 20);
        let id = history(&mut app, 0, 0);
        let modes = &mut app.screens.get_mut(&id).unwrap().modes;
        modes.alt_screen = true;
        modes.mouse_reporting = true;
        modes.sgr_mouse = true;
        assert_eq!(
            sent(&wheel(&mut app, true, 5, 12)),
            vec![&ClientRequest::Input {
                session: id,
                data: b"\x1b[<64;6;3M".to_vec()
            }]
        );
        app.screens.get_mut(&id).unwrap().modes.mouse_reporting = false;
        assert_eq!(
            sent(&wheel(&mut app, false, 5, 12)),
            vec![&ClientRequest::Input {
                session: id,
                data: b"\x1b[B\x1b[B\x1b[B".to_vec()
            }],
            "a full-screen program without the mouse gets arrow keys"
        );
        assert_eq!(
            sent(&app.on_key(k(K::PageUp))),
            vec![&ClientRequest::Input {
                session: id,
                data: b"\x1b[5~".to_vec()
            }],
            "scroll back is its own PageUp"
        );
        assert!(!app.scrolling, "its keys stay its own");
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) -> Vec<Action> {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: M::NONE,
        })
    }

    fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) -> Vec<Action> {
        use ratatui::crossterm::event::MouseButton::Left;
        let mut actions = mouse(app, MouseEventKind::Down(Left), from.0, from.1);
        actions.extend(mouse(app, MouseEventKind::Drag(Left), to.0, to.1));
        actions.extend(mouse(app, MouseEventKind::Up(Left), to.0, to.1));
        actions
    }

    fn copies(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Copy(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The attached card's screen with `text` written on its third line.
    fn writing(app: &mut App, text: &str) -> SessionId {
        app.pane_area = Rect::new(0, 10, 80, 20);
        let id = history(app, 0, 0);
        let line = &mut app.screens.get_mut(&id).unwrap().lines[2];
        for (c, ch) in text.chars().enumerate() {
            line[c].ch = ch;
        }
        id
    }

    #[test]
    fn a_drag_over_the_pane_copies_on_release() {
        let (mut app, _) = app();
        let id = writing(&mut app, "hello world");
        let actions = drag(&mut app, (0, 12), (4, 12));
        assert_eq!(copies(&actions), vec!["hello"]);
        assert!(sent(&actions).is_empty(), "nothing reaches the session");
        let selection = app.selection.unwrap();
        assert_eq!(selection.session, id);
        assert_eq!((selection.anchor, selection.head), ((0, 2), (4, 2)));
    }

    fn toast_texts(app: &App) -> Vec<String> {
        app.toasts.items().map(|t| t.text.clone()).collect()
    }

    #[test]
    fn a_copy_shows_a_toast() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        drag(&mut app, (0, 12), (4, 12));
        assert_eq!(toast_texts(&app), ["✓ copied 5 characters"]);
    }

    #[test]
    fn a_copy_of_one_character_says_character() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        // `o` and the blank after it, which is dropped.
        drag(&mut app, (4, 12), (5, 12));
        assert_eq!(toast_texts(&app), ["✓ copied 1 character"]);
    }

    #[test]
    fn an_agent_that_waits_elsewhere_shows_a_toast() {
        let (mut app, s) = app();
        let other = s[2].id;
        let mut waiting = s[2].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        let toasts: Vec<_> = app.toasts.items().cloned().collect();
        assert_eq!(toasts.len(), 1);
        assert!(
            toasts[0].text.ends_with("waits for you"),
            "{}",
            toasts[0].text
        );
        assert_eq!(
            toasts[0].kind,
            ToastKind::Agent {
                session: other,
                status: AgentStatus::NeedsFeedback
            }
        );
    }

    #[test]
    fn the_card_you_type_into_shows_no_toast() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        let mut waiting = s[0].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn turning_toasts_off_keeps_the_copy_toast() {
        let (mut app, s) = app();
        app.config.notify.toasts = false;
        let mut waiting = s[2].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        assert!(app.toasts.is_empty(), "no agent toast");
        writing(&mut app, "hello world");
        drag(&mut app, (0, 12), (4, 12));
        assert_eq!(toast_texts(&app), ["✓ copied 5 characters"]);
    }

    #[test]
    fn a_click_goes_to_the_toast_under_it() {
        let (mut app, s) = app();
        app.screen = Rect::new(0, 0, 100, 40);
        let mut waiting = s[2].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        let r = app.toasts.rects(app.screen)[0];
        use ratatui::crossterm::event::MouseButton::Left;
        let actions = mouse(&mut app, MouseEventKind::Down(Left), r.x + 1, r.y + 1);
        mouse(&mut app, MouseEventKind::Up(Left), r.x + 1, r.y + 1);
        assert!(copies(&actions).is_empty());
        assert_eq!(app.selected, Some(s[2].id), "the card it was about");
        assert!(app.toasts.is_empty(), "the toast goes");
        assert_eq!(app.selection, None, "no selection starts under a toast");
    }

    /// `s[2]` waits for the user; returns a point on its toast.
    fn waiting_toast(app: &mut App, s: &[SessionInfo]) -> (u16, u16) {
        app.screen = Rect::new(0, 0, 100, 40);
        let mut waiting = s[2].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        let r = app.toasts.rects(app.screen)[0];
        (r.x + 1, r.y + 1)
    }

    fn click(app: &mut App, (column, row): (u16, u16)) -> Vec<Action> {
        use ratatui::crossterm::event::MouseButton::Left;
        let mut actions = mouse(app, MouseEventKind::Down(Left), column, row);
        actions.extend(mouse(app, MouseEventKind::Up(Left), column, row));
        actions
    }

    // An idle waiting agent sends nothing more: the click itself attaches its card.
    #[test]
    fn a_click_on_an_agent_toast_attaches_its_card_in_the_grid() {
        let (mut app, s) = app();
        let at = waiting_toast(&mut app, &s);
        let actions = click(&mut app, at);
        assert!(
            sent(&actions).contains(&&ClientRequest::Attach {
                session: s[2].id,
                cols: 80,
                rows: 20
            }),
            "{actions:?}"
        );
        assert_eq!(app.attached, Some(s[2].id));
        assert_eq!(app.mode, Mode::Grid);
    }

    // The next key must not go to a card other than the one typed into before.
    #[test]
    fn a_click_on_an_agent_toast_from_the_focus_lands_in_the_grid() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        let at = waiting_toast(&mut app, &s);
        let actions = click(&mut app, at);
        assert_eq!((app.selected, app.mode), (Some(s[2].id), Mode::Grid));
        assert!(sent(&actions).iter().any(|r| matches!(
            r,
            ClientRequest::Attach { session, .. } if *session == s[2].id
        )));
    }

    #[test]
    fn a_click_on_an_agent_toast_leaves_the_archive_view() {
        let (mut app, s) = app();
        let at = waiting_toast(&mut app, &s);
        app.set_archive_view(true);
        click(&mut app, at);
        assert!(!app.archive_view());
        assert_eq!(app.selected, Some(s[2].id));
    }

    // Under an overlay the selection never moves: the click only dismisses the toast.
    #[test]
    fn under_an_overlay_a_click_only_dismisses_the_toast() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('p')));
        let before = app.selected;
        let at = waiting_toast(&mut app, &s);
        let actions = click(&mut app, at);
        assert!(app.toasts.is_empty(), "the toast goes");
        assert_eq!(app.selected, before);
        assert!(sent(&actions).is_empty(), "{actions:?}");
        assert_eq!(app.overlays.len(), 1);
    }

    #[test]
    fn toasts_go_by_themselves() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        drag(&mut app, (0, 12), (4, 12));
        let until = app.toasts.next_expiry().unwrap();
        assert!(app.next_wake(Instant::now()).is_some_and(|w| w <= until));
        app.tick(until);
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn a_program_that_wants_the_mouse_does_not_get_the_drag() {
        let (mut app, _) = app();
        let id = writing(&mut app, "hello world");
        let modes = &mut app.screens.get_mut(&id).unwrap().modes;
        modes.alt_screen = true;
        modes.mouse_reporting = true;
        modes.sgr_mouse = true;
        app.on_key(k(K::Enter));
        assert_eq!(app.mode, Mode::Focus);
        let actions = drag(&mut app, (6, 12), (10, 12));
        assert_eq!(copies(&actions), vec!["world"]);
        assert!(sent(&actions).is_empty());
    }

    #[test]
    fn a_click_without_a_drag_clears_the_selection() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        drag(&mut app, (0, 12), (4, 12));
        let actions = drag(&mut app, (7, 12), (7, 12));
        assert!(copies(&actions).is_empty());
        assert_eq!(app.selection, None);
    }

    #[test]
    fn a_drag_past_the_pane_stops_at_its_edge() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        drag(&mut app, (6, 12), (200, 60));
        assert_eq!(app.selection.unwrap().head, (79, 19));
    }

    #[test]
    fn a_drag_that_starts_off_the_pane_selects_nothing() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        let actions = drag(&mut app, (0, 3), (4, 12));
        assert!(copies(&actions).is_empty());
        assert_eq!(app.selection, None);
    }

    #[test]
    fn a_key_or_the_wheel_clears_the_selection() {
        let (mut app, _) = app();
        writing(&mut app, "hello world");
        drag(&mut app, (0, 12), (4, 12));
        app.on_key(k(K::Char('j')));
        assert_eq!(app.selection, None, "a key");
        drag(&mut app, (0, 12), (4, 12));
        wheel(&mut app, true, 5, 12);
        assert_eq!(app.selection, None, "the wheel moves what is under it");
    }

    #[test]
    fn another_card_or_ctrl_q_ends_scrolling() {
        let (mut app, s) = app();
        history(&mut app, 0, 500);
        app.on_key(k(K::PageUp));
        assert!(app.scrolling);
        app.on_key(k(K::Char('q')));
        app.on_key(k(K::Char('l')));
        assert_eq!(app.attached, Some(s[1].id));
        history(&mut app, 0, 500);
        app.on_key(k(K::PageUp));
        assert!(app.scrolling);
        app.on_key(k(K::Char('q')));
        app.on_key(k(K::Char('h')));
        assert!(!app.scrolling);

        let (mut app, _) = self::app();
        history(&mut app, 0, 500);
        app.on_key(k(K::PageUp));
        app.on_key(ctrl('q'));
        assert!(!app.scrolling, "C-q gets out of scrolling too");
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
    // sessions of a closed project are out of reach of the palette and of `.` / `,`
    // (unless one waits on the user; see below).
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
        let mut answered = s[3].clone();
        answered.status = AgentStatus::Running;
        app.on_event(ServerEvent::SessionUpdated(answered));
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

    /// `app()` with "web" closed; besides its waiting "w1" it has a running "w2".
    fn web_closed() -> (App, Vec<SessionInfo>) {
        let (mut app, mut s) = app();
        let mut w2 = session(s[3].project, "w2", AgentStatus::Running);
        w2.last_activity_ms = 0;
        app.on_event(ServerEvent::SessionUpdated(w2.clone()));
        s.push(w2);
        close_web(&mut app);
        (app, s)
    }

    #[test]
    fn waiting_agents_of_closed_projects_are_counted() {
        let (mut app, s) = web_closed();
        assert_eq!(app.waiting_in_closed_projects().len(), 1);
        app.on_event(ServerEvent::SessionUpdated(archived(
            &s[3],
            AgentStatus::NeedsFeedback,
        )));
        assert!(
            app.waiting_in_closed_projects().is_empty(),
            "not archived ones"
        );
    }

    // A closed project's agent waiting on the user is still reachable: landing on it
    // opens its project, and the card is selected once the project is open.
    #[test]
    fn dot_reaches_a_waiting_agent_of_a_closed_project_and_opens_it() {
        let (mut app, s) = web_closed();
        assert_eq!(app.selected, Some(s[0].id));
        let actions = app.on_key(k(K::Char('.')));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::OpenProject {
                project: s[3].project
            }]
        );
        assert_eq!(app.project, Some(s[0].project), "not until it is open");
        let mut state = app.state.clone();
        state.projects[1].open = true;
        app.on_event(ServerEvent::State(state));
        assert_eq!(
            (app.project, app.selected),
            (Some(s[3].project), Some(s[3].id))
        );
    }

    // The card on its way counts as where you are: the next `.` steps on from it.
    #[test]
    fn a_second_dot_steps_on_from_the_waiting_card_on_its_way() {
        let (mut app, s) = web_closed();
        app.on_key(k(K::Char('.')));
        let actions = app.on_key(k(K::Char('.')));
        assert!(
            !sent(&actions)
                .iter()
                .any(|r| matches!(r, ClientRequest::OpenProject { .. })),
            "{actions:?}"
        );
        assert_eq!(app.selected, Some(s[2].id), "the one after w1");
        let mut state = app.state.clone();
        state.projects[1].open = true;
        app.on_event(ServerEvent::State(state));
        assert_eq!(app.selected, Some(s[2].id), "and no jump when web opens");
    }

    #[test]
    fn other_sessions_of_a_closed_project_stay_out_of_reach() {
        let (mut app, s) = web_closed();
        // backwards from a1: a2, a3, then w1; w2 (running, before a2) is skipped
        app.on_key(k(K::Char(',')));
        assert_eq!(app.selected, Some(s[1].id));
        let actions = app.on_key(k(K::Char(',')));
        assert_eq!(app.selected, Some(s[2].id));
        assert!(
            !sent(&actions)
                .iter()
                .any(|r| matches!(r, ClientRequest::OpenProject { .. }))
        );
    }

    #[test]
    fn prefix_dot_to_a_closed_projects_waiting_agent_focuses_it_once_open() {
        let (mut app, s) = web_closed();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        assert_eq!(
            sent(&app.on_key(k(K::Char('.')))),
            vec![&ClientRequest::OpenProject {
                project: s[3].project
            }]
        );
        assert_eq!((app.selected, app.mode), (Some(s[0].id), Mode::Focus));
        let mut state = app.state.clone();
        state.projects[1].open = true;
        let actions = app.on_event(ServerEvent::State(state));
        assert_eq!((app.selected, app.mode), (Some(s[3].id), Mode::Focus));
        assert!(sent(&actions).contains(&&ClientRequest::Attach {
            session: s[3].id,
            cols: 80,
            rows: 20
        }));
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
                    canonical: under.join(name),
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

    // The daemon opens a project under its real path, so the browser asks for that
    // path and waits for exactly it.
    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_link_is_added_under_its_real_path() {
        let (mut app, _) = app();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), crate::browse::list_dir(&root));
        type_text(&mut app, "link");
        let actions = app.on_key(k(K::Tab));
        assert_eq!(
            sent(&actions),
            vec![&ClientRequest::AddProject {
                path: root.join("real")
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

    // Nothing new would open, so there is nothing to wait for.
    #[test]
    fn a_link_to_an_open_project_brings_its_tab_forward_at_once() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('o')));
        app.listed(
            Path::new("/"),
            Ok(Listing {
                entries: vec![crate::browse::DirEntry {
                    name: "web-link".into(),
                    path: "/code/web-link".into(),
                    canonical: "/web".into(),
                    git: true,
                }],
                truncated: false,
            }),
        );
        type_text(&mut app, "link");
        let actions = app.on_key(k(K::Tab));
        assert!(
            !sent(&actions).iter().any(|r| matches!(
                r,
                ClientRequest::AddProject { .. } | ClientRequest::OpenProject { .. }
            )),
            "{actions:?}"
        );
        assert!(app.overlays.is_empty());
        assert_eq!(
            (app.project, app.selected),
            (Some(s[3].project), Some(s[3].id))
        );
    }

    /// `o`, then Tab on a folder "orbit" that is not a project yet.
    fn add_orbit(app: &mut App) {
        app.on_key(k(K::Char('o')));
        app.listed(Path::new("/"), listing(&[("orbit", true)], Path::new("/")));
        type_text(app, "orbit");
        app.on_key(k(K::Tab));
    }

    /// The state with one more open project at `path`.
    fn opened(app: &mut App, path: &str) -> ProjectId {
        let mut state = app.state.clone();
        let id = ProjectId::new();
        state.projects.push(ProjectInfo {
            id,
            name: path.trim_start_matches('/').into(),
            path: path.into(),
            open: true,
        });
        app.on_event(ServerEvent::State(state));
        id
    }

    #[test]
    fn a_folder_being_added_does_not_jump_to_another_project_that_opens() {
        let (mut app, s) = app();
        add_orbit(&mut app);
        opened(&mut app, "/elsewhere");
        assert_eq!(app.project, Some(s[0].project), "opened by someone else");
        let orbit = opened(&mut app, "/orbit");
        assert_eq!(app.project, Some(orbit));
    }

    #[test]
    fn opening_the_archive_view_drops_the_switch_to_a_folder_being_added() {
        let (mut app, s) = app();
        add_orbit(&mut app);
        app.on_key(k(K::Char('A')));
        opened(&mut app, "/orbit");
        assert_eq!(app.project, Some(s[0].project));
        assert!(app.archive_view());
    }

    #[test]
    fn changing_tab_yourself_drops_the_switch_to_a_folder_being_added() {
        let keys: [&[KeyEvent]; 6] = [
            &[k(K::Char(']'))],
            &[k(K::Char('['))],
            &[k(K::Char('2'))],
            &[k(K::Char('.'))],
            &[k(K::Char(','))],
            &[k(K::Char('/')), k(K::Enter)],
        ];
        for sequence in keys {
            let (mut app, _) = app();
            add_orbit(&mut app);
            for key in sequence {
                app.on_key(*key);
            }
            let chosen = app.project;
            opened(&mut app, "/orbit");
            assert_eq!(app.project, chosen, "{sequence:?}");
        }
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
        app.set_card_window(2, rows as u16 * crate::ui::CARD_H);
        (app, s)
    }

    #[test]
    fn the_view_scrolls_to_keep_the_selected_card_on_screen() {
        let (mut app, s) = many(10, 2);
        for _ in 0..3 {
            app.on_key(k(K::Char('j')));
        }
        app.set_card_window(2, 2 * crate::ui::CARD_H);
        assert_eq!(app.selected, Some(s[6].id));
        assert_eq!(app.card_scroll, 2, "row 3 is the last row on screen");
        for _ in 0..3 {
            app.on_key(k(K::Char('k')));
        }
        app.set_card_window(2, 2 * crate::ui::CARD_H);
        assert_eq!(app.card_scroll, 0);
    }

    /// A project with a card on pull request #212's branch and one on main.
    fn linked() -> (App, Vec<SessionInfo>, PrRef) {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: "/w/site".into(),
            open: true,
        };
        let pr = PrRef {
            repo: termist_core::github::RepoId(7),
            number: 212,
        };
        let at = |name: &str, root: &str, pr: Option<PrRef>| {
            let mut s = session(p.id, name, AgentStatus::Finished);
            s.cwd = root.into();
            s.place = Some(Box::new(termist_core::Place {
                root: root.into(),
                branch: Some("b".into()),
                commit: None,
                repo: Some(termist_core::github::RepoId(7)),
                pr,
                gone: false,
            }));
            s
        };
        let s = vec![
            at("main", "/w/site", None),
            at("fix", "/w/site-worktrees/fix", Some(pr)),
        ];
        let mut app = App::new();
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p.clone()],
            sessions: s.clone(),
            ..StateSnapshot::default()
        }));
        let listed = vec![
            crate::prs::fixtures::summary(198, "Add a filter", "carol"),
            crate::prs::fixtures::summary(212, "Fix login", "bob"),
        ];
        app.on_event(ServerEvent::Prs {
            project: p.id,
            state: GhState::Ok,
            discovered: 1,
            repos: vec![crate::prs::fixtures::repo(7, "site", listed)],
        });
        (app, s, pr)
    }

    #[test]
    fn w_opens_a_worktree_and_the_quick_prompt_starts_the_agent_there() {
        let (mut app, s, pr) = linked();
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Claude,
            available: true,
        }]));
        app.select(s[1].id);
        app.on_key(k(K::Char('v')));
        assert_eq!(
            sent(&app.on_key(k(K::Char('w')))),
            [&ClientRequest::OpenWorktree { pr }]
        );
        assert_eq!(app.message.as_deref(), Some("opening a worktree for #212…"));
        let other = PrRef { number: 198, ..pr };
        app.on_event(ServerEvent::WorktreeReady {
            pr: other,
            path: "/w/elsewhere".into(),
            created: true,
        });
        assert!(app.overlays.is_empty(), "not the one asked for");
        let path = PathBuf::from("/w/site-worktrees/feat");
        app.on_event(ServerEvent::WorktreeReady {
            pr,
            path: path.clone(),
            created: true,
        });
        let Some(Overlay::QuickPrompt(q)) = app.overlays.last() else {
            panic!("the quick prompt")
        };
        assert_eq!(q.worktree, Some((path.clone(), "feat".to_string())));
        assert_eq!(
            q.input.text(),
            "Pull request #212 \"Fix login\" (https://github.com/acme/site/pull/212): feat into main. "
        );
        assert!(crate::overlay_view::launch_line(&app, q).starts_with("site ^P · ⎇ feat ^T"));
        let actions = app.on_key(k(K::Enter));
        let created = sent(&actions).into_iter().find_map(|r| match r {
            ClientRequest::CreateSession { cwd, .. } => Some(cwd.clone()),
            _ => None,
        });
        assert_eq!(created, Some(Some(path)));
        assert_eq!(app.prompt_draft, None);
    }

    #[test]
    fn a_worktree_that_cannot_be_made_says_why_where_you_are() {
        let (mut app, s, pr) = linked();
        app.select(s[1].id);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('w')));
        let why = "couldn't open a worktree · fatal: invalid reference";
        app.on_event(ServerEvent::WorktreeFailed {
            pr,
            message: why.into(),
        });
        assert_eq!(app.message.as_deref(), Some(why));
        app.on_key(k(K::Char('w')));
        app.on_key(k(K::Char('v'))); // back to the grid while it is on its way
        app.on_event(ServerEvent::WorktreeFailed {
            pr,
            message: why.into(),
        });
        let texts: Vec<String> = app.toasts.items().map(|t| t.text.clone()).collect();
        assert_eq!(
            texts,
            [format!("✗ {why}")],
            "a toast where the list is gone"
        );
    }

    #[test]
    fn marks_wait_for_their_own_hand_over_not_any_send() {
        let (mut app, _, pr) = linked();
        let summary = crate::prs::fixtures::summary(212, "Fix login", "bob");
        app.on_event(ServerEvent::PrDetail {
            pr,
            state: GhState::Ok,
            detail: Some(Box::new(crate::prs::fixtures::detail(summary))),
        });
        app.marks.insert(pr, BTreeSet::from(["T1".to_string()]));
        app.hand(pr, None);
        assert_eq!(app.hand_for, Some(pr), "on its way to a new agent");
        app.on_event(ServerEvent::WorktreeFailed {
            pr,
            message: "couldn't open a worktree · no".into(),
        });
        assert_eq!(app.hand_for, None, "a failed worktree hands nothing over");
        app.hand(pr, None);
        app.open_quick_prompt();
        assert_eq!(app.hand_for, None, "a plain new task is not the hand-over");
        app.overlays.clear();
        app.hand(pr, None);
        app.selected = Some(app.state.sessions[0].id);
        app.open_follow_up();
        assert_eq!(app.hand_for, None, "nor is a plain follow-up");
        assert!(app.marks.contains_key(&pr), "still marked");
    }

    fn quick(app: &App) -> &QuickPrompt {
        match app.overlays.last() {
            Some(Overlay::QuickPrompt(q)) => q,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ctrl_n_makes_a_worktree_named_from_the_prompt_and_starts_the_task_in_it() {
        let (mut app, s, _) = linked();
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Claude,
            available: true,
        }]));
        app.select(s[0].id);
        app.on_key(k(K::Char('p')));
        assert_eq!(quick(&app).worktree, None, "a card in the project's folder");
        for c in "Giriş sayfasını düzelt".chars() {
            app.on_key(k(K::Char(c)));
        }
        app.on_key(ctrl('n'));
        assert_eq!(
            app.new_branch(quick(&app)).as_deref(),
            Some("giris-sayfasini-duzelt")
        );
        let actions = app.on_key(k(K::Enter));
        let ticket = match sent(&actions)[..] {
            [
                ClientRequest::CreateWorktree {
                    repo,
                    branch,
                    ticket,
                    ..
                },
            ] => {
                assert_eq!((repo.0, branch.as_str()), (7, "giris-sayfasini-duzelt"));
                *ticket
            }
            ref other => panic!("{other:?}"),
        };
        assert_eq!(
            app.message.as_deref(),
            Some("making a worktree giris-sayfasini-duzelt…")
        );
        app.on_event(ServerEvent::WorktreeNotMade {
            ticket,
            message: "couldn't make a worktree · not a branch name: x".into(),
        });
        assert_eq!(
            quick(&app).input.text(),
            "Giriş sayfasını düzelt",
            "the words come back"
        );
        let actions = app.on_key(k(K::Enter));
        let ticket = sent(&actions)
            .iter()
            .find_map(|r| match r {
                ClientRequest::CreateWorktree { ticket, .. } => Some(*ticket),
                _ => None,
            })
            .unwrap();
        let actions = app.on_event(ServerEvent::WorktreeMade {
            ticket,
            path: "/w/site-worktrees/giris-sayfasini-duzelt".into(),
            branch: "giris-sayfasini-duzelt".into(),
            note: Some("made from local main: no fetch from origin".into()),
        });
        let started = sent(&actions)
            .into_iter()
            .find_map(|r| match r {
                ClientRequest::CreateSession { cwd, prompt, .. } => {
                    Some((cwd.clone(), prompt.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            started,
            (
                Some("/w/site-worktrees/giris-sayfasini-duzelt".into()),
                Some("Giriş sayfasını düzelt".into())
            )
        );
        assert_eq!(
            app.message.as_deref(),
            Some("made from local main: no fetch from origin")
        );
    }

    fn local(app: &App) -> &crate::diff::local::LocalView {
        match &app.view {
            View::Diff(l) => l,
            other => panic!("{other:?}"),
        }
    }

    fn folder_diff(files: &[&str]) -> termist_core::LocalDiffData {
        termist_core::LocalDiffData {
            head: "fix".into(),
            base: "origin/main".into(),
            dirty: true,
            files: files
                .iter()
                .map(|p| termist_core::github::DiffFile {
                    path: p.to_string(),
                    previous: None,
                    change: 'M',
                    additions: 1,
                    deletions: 0,
                    viewed: termist_core::github::Viewed::Unviewed,
                    patch: termist_core::github::Patch::Text("@@ -1 +1,2 @@\n a\n+b".into()),
                    url: String::new(),
                })
                .collect(),
            more: 0,
        }
    }

    #[test]
    fn g_opens_the_card_s_worktree_diff_and_esc_leaves_it_on_the_card() {
        use termist_core::{DiffMode, ReadState};
        let (mut app, s, _) = linked();
        app.select(s[1].id);
        let fix = std::path::PathBuf::from("/w/site-worktrees/fix");
        let actions = app.on_key(k(K::Char('g')));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetLocalDiff {
                path: Some(fix.clone()),
                mode: DiffMode::Branch
            }]
        );
        assert_eq!(local(&app).state, ReadState::Reading);
        // Another folder's or another mode's diff is not this one.
        app.on_event(ServerEvent::LocalDiff {
            path: "/w/site".into(),
            mode: DiffMode::Branch,
            state: ReadState::Ready,
            diff: Some(Box::new(folder_diff(&["x.rs"]))),
        });
        assert!(local(&app).diff.is_none());
        app.on_event(ServerEvent::LocalDiff {
            path: fix.clone(),
            mode: DiffMode::Branch,
            state: ReadState::Ready,
            diff: Some(Box::new(folder_diff(&["a.rs", "b.rs"]))),
        });
        assert_eq!(local(&app).view.file.as_deref(), Some("a.rs"));
        let actions = app.on_key(ctrl('r'));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetReviewed {
                worktree: fix.clone(),
                file: "a.rs".into(),
                reviewed: true
            }]
        );
        assert_eq!(
            local(&app).view.file.as_deref(),
            Some("b.rs"),
            "on to the next"
        );
        app.on_key(k(K::Char('c')));
        assert_eq!(app.message.as_deref(), Some("not in a local diff"));
        let actions = app.on_key(k(K::Char('u')));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetLocalDiff {
                path: Some(fix.clone()),
                mode: DiffMode::Uncommitted
            }]
        );
        assert!(local(&app).diff.is_none(), "read anew");
        let actions = app.on_key(k(K::Char('R')));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetLocalDiff {
                path: Some(fix.clone()),
                mode: DiffMode::Uncommitted
            }],
            "read again now"
        );
        let actions = app.on_key(k(K::Esc));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetLocalDiff {
                path: None,
                mode: DiffMode::Branch
            }]
        );
        assert_eq!(app.view, View::Grid);
        assert_eq!(app.selected, Some(s[1].id), "on the card it came from");
    }

    fn tool_card(project: ProjectId, name: &str, program: &str) -> SessionInfo {
        let mut s = session(project, name, AgentStatus::Fresh);
        s.kind = SessionKind::Tool {
            program: program.into(),
            args: vec![],
        };
        s
    }

    #[test]
    fn l_opens_lazygit_in_the_card_s_folder_and_its_end_comes_back_to_the_card() {
        let (mut app, s, _) = linked();
        let project = app.state.projects[0].id;
        app.select(s[1].id);
        let actions = app.on_key(k(K::Char('L')));
        assert!(matches!(
            sent(&actions)[..],
            [ClientRequest::CreateSession { kind: SessionKind::Tool { program, .. }, cwd: Some(cwd), .. }]
                if program == "lazygit" && cwd.as_path() == std::path::Path::new("/w/site-worktrees/fix")
        ));
        let tool = tool_card(project, "lazygit-3", "lazygit");
        app.on_event(ServerEvent::SessionUpdated(tool.clone()));
        assert_eq!(
            (app.selected, app.mode),
            (Some(tool.id), Mode::Focus),
            "typed into at once"
        );
        app.on_event(ServerEvent::SessionRemoved(tool.id));
        assert_eq!(
            (app.selected, app.mode),
            (Some(s[1].id), Mode::Grid),
            "back on the card it was opened from"
        );
    }

    #[test]
    fn o_opens_the_folder_in_the_editor_and_a_terminal_one_comes_as_a_card() {
        let (mut app, s, _) = linked();
        let project = app.state.projects[0].id;
        app.select(s[1].id);
        app.config.editor = Some("code".into());
        let actions = app.on_key(k(K::Char('O')));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::OpenInEditor {
                project,
                folder: "/w/site-worktrees/fix".into(),
                file: None,
                line: None,
                editor: Some("code".into()),
            }]
        );
        assert!(
            !app.focus_next_created,
            "a window of its own: no card to wait for"
        );
        app.config.editor = Some("nvim".into());
        app.on_key(k(K::Char('O')));
        assert!(app.focus_next_created, "a terminal editor comes as a card");
        app.on_event(ServerEvent::EditorFailed {
            message: "nvim not found".into(),
        });
        assert!(!app.focus_next_created);
        assert!(app.toasts.items().any(|t| t.text == "✗ nvim not found"));
    }

    fn finder(app: &App) -> &crate::finder::Finder {
        match app.overlays.last() {
            Some(Overlay::Finder(f)) => f,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn f_lists_the_repo_s_files_and_enter_opens_one_in_the_editor() {
        let (mut app, s, _) = linked();
        let project = app.state.projects[0].id;
        app.select(s[1].id);
        app.config.editor = Some("code".into());
        let fix = std::path::PathBuf::from("/w/site-worktrees/fix");
        let actions = app.on_key(k(K::Char('f')));
        let ticket = match sent(&actions)[..] {
            [ClientRequest::ListFiles { folder, ticket }] if *folder == fix => *ticket,
            ref other => panic!("{other:?}"),
        };
        let files = |ticket| ServerEvent::Files {
            ticket,
            root: fix.clone(),
            files: vec!["src/auth.rs".into(), "src/login.rs".into()],
            more: 0,
        };
        app.on_event(files(ticket + 100));
        assert!(finder(&app).files.is_none(), "an answer to another ask");
        app.on_event(files(ticket));
        assert_eq!(finder(&app).hits.len(), 2);
        typed(&mut app, "log");
        assert_eq!(finder(&app).hits.len(), 1);
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::OpenInEditor {
                project,
                folder: fix.clone(),
                file: Some(fix.join("src/login.rs").display().to_string()),
                line: None,
                editor: Some("code".into()),
            }]
        );
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn shift_f_asks_git_grep_a_moment_after_typing_and_tab_switches_to_files() {
        let (mut app, s, _) = linked();
        app.select(s[1].id);
        app.config.editor = Some("code".into());
        let fix = std::path::PathBuf::from("/w/site-worktrees/fix");
        assert!(
            sent(&app.on_key(k(K::Char('F')))).is_empty(),
            "nothing to look for yet"
        );
        typed(&mut app, "re");
        let now = Instant::now();
        assert!(app.due(now).is_empty(), "a moment after the last key");
        let actions = app.due(now + crate::finder::WAIT * 2);
        let ticket = match sent(&actions)[..] {
            [
                ClientRequest::Grep {
                    folder,
                    query,
                    ticket,
                },
            ] if *folder == fix && query == "re" => *ticket,
            ref other => panic!("{other:?}"),
        };
        app.on_event(ServerEvent::GrepResults {
            ticket,
            root: fix.clone(),
            matches: vec![termist_core::GrepMatch {
                path: "src/auth.rs".into(),
                line: 42,
                text: "let redirect = q;".into(),
            }],
            more: false,
        });
        assert_eq!(finder(&app).hits.len(), 1);
        let actions = app.on_key(k(K::Tab));
        assert!(matches!(
            sent(&actions)[..],
            [ClientRequest::ListFiles { .. }]
        ));
        assert_eq!(finder(&app).kind, crate::finder::FindKind::Files);
        assert_eq!(finder(&app).query, "re", "the words stay");
        app.on_key(k(K::Tab));
        let actions = app.due(Instant::now() + crate::finder::WAIT * 2);
        assert!(
            matches!(sent(&actions)[..], [ClientRequest::Grep { .. }]),
            "asked again"
        );
        app.on_key(k(K::Esc));
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn a_finder_on_a_folder_that_is_no_repo_closes_and_says_so() {
        let (mut app, s, _) = linked();
        app.select(s[0].id);
        let actions = app.on_key(k(K::Char('f')));
        let ticket = match sent(&actions)[..] {
            [ClientRequest::ListFiles { ticket, .. }] => *ticket,
            ref other => panic!("{other:?}"),
        };
        app.on_event(ServerEvent::FindFailed {
            ticket,
            message: "not a git repository".into(),
        });
        assert!(app.overlays.is_empty());
        assert!(
            app.toasts
                .items()
                .any(|t| t.text == "✗ not a git repository")
        );
    }

    #[test]
    fn g_on_a_folder_that_is_no_repo_says_so_and_stays_on_the_grid() {
        use termist_core::{DiffMode, ReadState};
        let (mut app, s, _) = linked();
        app.select(s[0].id);
        app.on_key(k(K::Char('g')));
        let actions = app.on_event(ServerEvent::LocalDiff {
            path: "/w/site".into(),
            mode: DiffMode::Branch,
            state: ReadState::Failed("not a git repository".into()),
            diff: None,
        });
        assert_eq!(app.view, View::Grid);
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetLocalDiff {
                path: None,
                mode: DiffMode::Branch
            }],
            "nothing to watch"
        );
        assert!(
            app.toasts
                .items()
                .any(|t| t.text == "✗ not a git repository"),
            "a toast says why"
        );
        assert_eq!(app.selected, Some(s[0].id));
    }

    #[test]
    fn g_on_a_card_in_the_project_s_folder_or_a_stand_in_or_in_focus() {
        use termist_core::DiffMode;
        let (mut app, s, _) = linked();
        let asked = |actions: &[Action]| {
            sent(actions).into_iter().find_map(|r| match r {
                ClientRequest::SetLocalDiff { path, .. } => path.clone(),
                _ => None,
            })
        };
        app.select(s[0].id);
        assert_eq!(asked(&app.on_key(k(K::Char('g')))), Some("/w/site".into()));
        app.on_key(k(K::Char('q')));
        assert_eq!(app.view, View::Grid, "q leaves too");
        // In focus mode: the prefix, then g.
        app.select(s[1].id);
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        assert_eq!(
            asked(&app.on_key(k(K::Char('g')))),
            Some("/w/site-worktrees/fix".into())
        );
        assert_eq!(app.mode, Mode::Grid, "the diff takes the keys");
        app.on_key(k(K::Esc));
        // A band's stand-in.
        app.selected = None;
        app.empty = Some("/w/site-worktrees/docs".into());
        app.on_event(ServerEvent::Worktrees {
            project: app.state.projects[0].id,
            list: vec![worktree("/w/site-worktrees/docs", "docs", true, true)],
        });
        assert_eq!(
            asked(&app.on_key(k(K::Char('g')))),
            Some("/w/site-worktrees/docs".into())
        );
        let _ = DiffMode::Branch;
    }

    #[test]
    fn two_new_worktrees_on_their_way_each_start_their_own_task() {
        let (mut app, _, _) = linked();
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Claude,
            available: true,
        }]));
        let mut tickets = vec![];
        for text in ["first task", "second task"] {
            app.on_key(k(K::Char('p')));
            app.on_key(ctrl('u'));
            for c in text.chars() {
                app.on_key(k(K::Char(c)));
            }
            app.on_key(ctrl('n'));
            let actions = app.on_key(k(K::Enter));
            tickets.push(
                sent(&actions)
                    .iter()
                    .find_map(|r| match r {
                        ClientRequest::CreateWorktree { ticket, .. } => Some(*ticket),
                        _ => None,
                    })
                    .unwrap(),
            );
        }
        let mut prompts = vec![];
        for (ticket, branch) in tickets.into_iter().zip(["first-task", "second-task"]) {
            let actions = app.on_event(ServerEvent::WorktreeMade {
                ticket,
                path: format!("/w/site-worktrees/{branch}").into(),
                branch: branch.into(),
                note: None,
            });
            prompts.extend(sent(&actions).into_iter().filter_map(|r| match r {
                ClientRequest::CreateSession { prompt, .. } => prompt.clone(),
                _ => None,
            }));
        }
        assert_eq!(prompts, ["first task", "second task"], "neither is lost");
    }

    #[test]
    fn the_quick_prompt_starts_where_the_selection_is_and_ctrl_t_changes_it() {
        let (mut app, s, _) = linked();
        app.on_event(ServerEvent::Worktrees {
            project: app.state.projects[0].id,
            list: vec![termist_core::WorktreeInfo {
                path: "/w/site-worktrees/fix".into(),
                repo: Some(termist_core::github::RepoId(7)),
                branch: Some("fix".into()),
                base: Some("main".into()),
                made_by_termist: true,
                shown: true,
                stat: None,
                pr_end: None,
                reviewed: 0,
            }],
        });
        app.select(s[1].id);
        app.on_key(k(K::Char('p')));
        assert_eq!(
            quick(&app).worktree,
            Some(("/w/site-worktrees/fix".into(), "b".to_string())),
            "the selected card's worktree"
        );
        app.on_key(ctrl('t'));
        let Some(Overlay::Target(picker)) = app.overlays.last() else {
            panic!("where to")
        };
        let labels: Vec<&str> = (0..picker.items().len()).map(|i| picker.label(i)).collect();
        assert_eq!(
            labels,
            ["site · the project's folder", "⎇ fix", "new worktree…"]
        );
        app.on_key(k(K::Enter));
        assert_eq!(quick(&app).worktree, None, "the project's folder");
        app.on_key(ctrl('t'));
        app.on_key(k(K::Down));
        app.on_key(k(K::Down));
        app.on_key(k(K::Enter));
        assert!(quick(&app).new_worktree.is_some(), "a new one");
        app.on_key(k(K::Esc));
        app.config.agents.new_worktree_by_default = true;
        app.select(s[0].id);
        app.on_key(k(K::Char('p')));
        assert!(
            quick(&app).new_worktree.is_some(),
            "new by default from the folder"
        );
    }

    #[test]
    fn shift_p_starts_a_task_like_the_card_and_t_opens_a_shell_in_its_worktree() {
        let (mut app, s, _) = linked();
        let mut agent = s[1].clone();
        agent.kind = SessionKind::Agent {
            harness: Harness::Codex,
        };
        agent.model = Some("gpt-5".into());
        agent.effort = Some("high".into());
        app.on_event(ServerEvent::SessionUpdated(agent.clone()));
        app.select(agent.id);
        app.on_key(k(K::Char('P')));
        let q = quick(&app);
        assert_eq!(
            (
                q.launch.harness,
                q.launch.model.as_deref(),
                q.launch.effort.as_deref()
            ),
            (Harness::Codex, Some("gpt-5"), Some("high"))
        );
        assert_eq!(
            q.worktree,
            Some(("/w/site-worktrees/fix".into(), "b".to_string()))
        );
        assert_eq!(q.input.text(), "");
        app.on_key(k(K::Esc));
        let shell_in = |app: &mut App| {
            sent(&app.on_key(k(K::Char('t'))))
                .into_iter()
                .find_map(|r| match r {
                    ClientRequest::CreateSession { cwd, .. } => Some(cwd.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(shell_in(&mut app), Some("/w/site-worktrees/fix".into()));
        app.select(s[0].id);
        assert_eq!(shell_in(&mut app), None, "the project's folder");
        app.selected = None;
        app.empty = Some("/w/site-worktrees/docs".into());
        app.on_event(ServerEvent::Worktrees {
            project: app.state.projects[0].id,
            list: vec![termist_core::WorktreeInfo {
                path: "/w/site-worktrees/docs".into(),
                repo: Some(termist_core::github::RepoId(7)),
                branch: Some("docs".into()),
                base: None,
                made_by_termist: true,
                shown: true,
                stat: None,
                pr_end: None,
                reviewed: 0,
            }],
        });
        assert_eq!(
            shell_in(&mut app),
            Some("/w/site-worktrees/docs".into()),
            "a band's stand-in"
        );
    }

    fn worktree(path: &str, branch: &str, shown: bool, made: bool) -> termist_core::WorktreeInfo {
        termist_core::WorktreeInfo {
            path: path.into(),
            repo: Some(termist_core::github::RepoId(7)),
            branch: Some(branch.into()),
            base: None,
            made_by_termist: made,
            shown,
            stat: None,
            pr_end: None,
            reviewed: 0,
        }
    }

    #[test]
    fn x_removes_a_worktree_after_asking_twice_when_work_would_go_with_it() {
        let (mut app, s, _) = linked();
        let path = PathBuf::from("/w/site-worktrees/fix");
        app.on_event(ServerEvent::Worktrees {
            project: app.state.projects[0].id,
            list: vec![worktree("/w/site-worktrees/fix", "fix", true, true)],
        });
        app.select(s[0].id);
        app.on_key(k(K::Char('X')));
        assert_eq!(
            app.message.as_deref(),
            Some("the project's own folder is not a worktree")
        );
        let mut live = s[1].clone();
        live.status = AgentStatus::Running;
        app.on_event(ServerEvent::SessionUpdated(live.clone()));
        app.select(live.id);
        app.on_key(k(K::Char('X')));
        assert_eq!(app.message.as_deref(), Some("stop its cards first (d)"));
        live.status = AgentStatus::Exited { code: Some(0) };
        app.on_event(ServerEvent::SessionUpdated(live));
        app.on_key(k(K::Char('X')));
        assert_eq!(app.mode, Mode::ConfirmRemove { files: 0 });
        assert_eq!(app.removing_name(), "fix");
        assert_eq!(
            sent(&app.on_key(k(K::Char('y')))),
            [&ClientRequest::RemoveWorktree {
                path: path.clone(),
                force: false
            }]
        );
        app.on_event(ServerEvent::RemoveRefused {
            path: path.clone(),
            files: 3,
        });
        assert_eq!(app.mode, Mode::ConfirmRemove { files: 3 });
        assert_eq!(
            sent(&app.on_key(k(K::Enter))),
            [&ClientRequest::RemoveWorktree {
                path: path.clone(),
                force: true
            }]
        );
        app.on_event(ServerEvent::WorktreeRemoved { path: path.clone() });
        assert_eq!(
            app.message.as_deref(),
            Some("removed fix · the branch stays")
        );
        // Any other key keeps it.
        app.removing = Some(path);
        app.mode = Mode::ConfirmRemove { files: 0 };
        assert!(sent(&app.on_key(k(K::Char('n')))).is_empty());
        assert_eq!((app.mode, app.removing.clone()), (Mode::Grid, None));
    }

    #[test]
    fn w_lists_the_worktrees_to_show_hide_or_remove() {
        let (mut app, _, _) = linked();
        app.on_event(ServerEvent::Worktrees {
            project: app.state.projects[0].id,
            list: vec![
                worktree("/w/site-worktrees/fix", "fix", true, true),
                worktree("/w/site/.claude/worktrees/x", "x", false, false),
            ],
        });
        app.on_key(k(K::Char('W')));
        let Some(Overlay::Worktrees(picker)) = app.overlays.last() else {
            panic!("the worktrees")
        };
        assert_eq!(
            (picker.label(0), picker.label(1)),
            ("✓ ⎇ fix · termist · has cards", "  ⎇ x · outside")
        );
        app.on_key(k(K::Enter));
        assert_eq!(app.message.as_deref(), Some("it has cards: always shown"));
        app.on_key(k(K::Down));
        assert_eq!(
            sent(&app.on_key(k(K::Char(' ')))),
            [&ClientRequest::SetWorktreeShown {
                path: "/w/site/.claude/worktrees/x".into(),
                shown: true
            }]
        );
        app.on_key(k(K::Char('X')));
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::ConfirmRemove { files: 0 });
    }

    #[test]
    fn shift_v_opens_the_card_s_pull_request_in_the_browser() {
        let (mut app, s, _) = linked();
        app.select(s[1].id);
        assert_eq!(
            app.on_key(k(K::Char('V'))),
            [Action::OpenUrl(
                "https://github.com/acme/site/pull/212".into()
            )]
        );
        app.select(s[0].id);
        assert!(app.on_key(k(K::Char('V'))).is_empty());
        assert_eq!(
            app.message.as_deref(),
            Some("no pull request for this branch")
        );
    }

    #[test]
    fn v_on_a_card_with_a_pull_request_opens_the_list_on_it_and_a_band_s_number_opens_it() {
        let (mut app, s, pr) = linked();
        app.select(s[1].id);
        app.on_key(k(K::Char('v')));
        let View::Prs(view) = &app.view else {
            panic!("the pull requests")
        };
        assert_eq!((view.selected, view.detail.is_none()), (Some(pr), true));
        app.on_key(k(K::Char('v')));
        app.select(s[0].id);
        app.on_key(k(K::Char('v')));
        let View::Prs(view) = &app.view else { panic!() };
        assert_ne!(view.selected, Some(pr), "a card without one: as before");
        app.on_key(k(K::Char('v')));
        app.hits
            .borrow_mut()
            .band_prs
            .push((pr, ratatui::layout::Rect::new(20, 6, 4, 1)));
        click(&mut app, (21, 6));
        let View::Prs(view) = &app.view else {
            panic!("the pull request")
        };
        assert_eq!(view.detail.as_ref().map(|d| d.pr), Some(pr));
    }

    #[test]
    fn j_and_k_go_band_by_band_and_l_goes_in_the_order_drawn() {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: "/w/site".into(),
            open: true,
        };
        let at = |name: &str, root: &str| {
            let mut s = session(p.id, name, AgentStatus::Finished);
            s.cwd = root.into();
            s.place = Some(Box::new(termist_core::Place {
                root: root.into(),
                branch: Some("b".into()),
                commit: None,
                repo: None,
                pr: None,
                gone: false,
            }));
            s
        };
        // Started in this order; drawn as main: m1 m2 m3, then the worktree: w1.
        let s = vec![
            at("m1", "/w/site"),
            at("w1", "/w/site-worktrees/fix"),
            at("m2", "/w/site"),
            at("m3", "/w/site"),
        ];
        let mut app = App::new();
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: s.clone(),
            ..StateSnapshot::default()
        }));
        app.set_card_window(2, 2 * (crate::ui::CARD_H + 1));
        app.select(s[0].id);
        for (key, want) in [
            ('l', 2),
            ('l', 3),
            ('l', 1),
            ('k', 3),
            ('k', 0),
            ('j', 3),
            ('j', 1),
        ] {
            app.on_key(k(K::Char(key)));
            assert_eq!(app.selected, Some(s[want].id), "{key} to {}", s[want].name);
        }
        let (per_row, lines) = (app.cards_per_row, app.card_lines);
        app.set_card_window(per_row, lines);
        assert_eq!(app.card_scroll, 1, "the worktree's row is on screen");
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
        assert!(app.archive_view());
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
        assert!(!app.archive_view());
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
        // Ctrl+Q is not here: it is global and closes everything.
        for c in ['a', 'p', 'r', 'o', 'x', '/', ' ', 'n', 't'] {
            assert!(sent(&app.on_key(ctrl(c))).is_empty(), "{c:?}");
            assert!(app.overlays.is_empty(), "{c:?}");
            assert_eq!(app.mode, Mode::Grid, "{c:?}");
        }
        let ctrl_shift_a = KeyEvent::new(K::Char('A'), M::CONTROL | M::SHIFT);
        app.on_key(ctrl_shift_a);
        assert!(!app.archive_view());
        app.on_key(k(K::Char('A')));
        app.on_key(ctrl_shift_a);
        assert!(app.archive_view(), "and it does not leave the archive view");
    }

    #[test]
    fn after_the_prefix_ctrl_with_p_or_slash_does_nothing() {
        let (mut app, _) = app();
        app.on_key(k(K::Enter));
        for c in ['p', '/'] {
            app.on_key(ctrl('a'));
            assert!(sent(&app.on_key(ctrl(c))).is_empty(), "{c:?}");
            assert!(app.overlays.is_empty(), "{c:?}");
            assert_eq!(app.mode, Mode::Focus, "{c:?}");
        }
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
        assert!(!app.archive_view());
        assert_eq!(app.selected, Some(s[0].id));
    }

    #[test]
    fn the_quick_prompt_starts_on_the_configured_cli_until_one_is_used() {
        let (mut app, _) = app();
        app.config.agents.default = Harness::Codex;
        app.state.last_launch = None;
        app.on_key(k(K::Char('p')));
        let Some(Overlay::QuickPrompt(q)) = app.overlays.last() else {
            panic!("no quick prompt");
        };
        assert_eq!(q.launch.harness, Harness::Codex);
    }

    #[test]
    fn keys_match_with_their_exact_modifiers() {
        let (mut app, _) = app();
        let selected = app.selected;
        let project = app.project;
        for c in ['h', 'j', 'k', 'l', '.', ',', ']', '[', '1', '2'] {
            assert!(sent(&app.on_key(ctrl(c))).is_empty(), "{c:?}");
            assert_eq!(app.selected, selected, "Ctrl+{c} does not move");
            assert_eq!(app.project, project, "Ctrl+{c} does not switch tabs");
        }
    }

    fn with_keys(grid: &[(&str, &str)], focus: &[(&str, &str)], prefix: &str) -> App {
        let (mut app, _) = app();
        let keys = termist_core::config::KeysConfig {
            grid: grid
                .iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect(),
            focus: focus
                .iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect(),
        };
        let (keymap, problems) = Keymap::from_config(&keys, prefix);
        assert!(problems.is_empty(), "{problems:?}");
        app.keymap = keymap;
        app
    }

    #[test]
    fn a_rebound_key_does_the_action_and_an_unbound_one_nothing() {
        let mut app = with_keys(&[("g", "quick_prompt"), ("p", "none")], &[], "C-a");
        app.on_key(k(K::Char('p')));
        assert!(app.overlays.is_empty(), "p is unbound");
        app.on_key(k(K::Char('g')));
        assert!(matches!(app.overlays.last(), Some(Overlay::QuickPrompt(_))));
    }

    #[test]
    fn another_prefix_leaves_ctrl_a_to_the_session() {
        let mut app = with_keys(&[], &[("z", "palette")], "C-Space");
        let id = app.selected.unwrap();
        app.on_key(k(K::Enter));
        let input = |data: Vec<u8>| ClientRequest::Input { session: id, data };
        assert_eq!(sent(&app.on_key(ctrl('a'))), vec![&input(vec![0x01])]);
        assert_eq!(app.mode, Mode::Focus);
        app.on_key(ctrl(' '));
        assert_eq!(app.mode, Mode::FocusPrefix);
        assert_eq!(sent(&app.on_key(ctrl(' '))), vec![&input(vec![0x00])]);
        app.on_key(ctrl(' '));
        app.on_key(k(K::Char('z')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Palette(_))));
    }

    #[test]
    fn after_the_prefix_n_and_t_start_a_session() {
        let (mut app, _) = app();
        app.on_key(k(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(k(K::Char('n')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Harness(_))));
        app.on_key(k(K::Esc));
        app.on_key(ctrl('a'));
        let actions = app.on_key(k(K::Char('t')));
        assert!(
            sent(&actions).iter().any(|r| matches!(
                r,
                ClientRequest::CreateSession {
                    kind: SessionKind::Shell,
                    ..
                }
            )),
            "{actions:?}"
        );
    }

    fn writes(actions: &[Action]) -> Vec<&ConfigEdit> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::WriteConfig(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn top_note(app: &App) -> Option<String> {
        match app.overlays.last() {
            Some(Overlay::Settings(v) | Overlay::Keys(v)) => v.note.clone(),
            Some(Overlay::KeyCapture(c)) => c.note.clone(),
            _ => None,
        }
    }

    #[test]
    fn the_theme_row_goes_through_every_theme() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        let ids: Vec<String> = app.themes.ids().map(String::from).collect();
        let mut seen = vec![app.config.theme.clone()];
        for _ in 1..ids.len() {
            app.on_key(k(K::Right));
            seen.push(app.config.theme.clone());
        }
        assert_eq!(seen, ids);
        app.on_key(k(K::Right));
        assert_eq!(app.config.theme, ids[0], "round again");
    }

    #[test]
    fn the_theme_changes_at_once_is_saved_and_agents_are_told() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Settings(_))));
        let actions = app.on_key(k(K::Right));
        assert_eq!(app.theme.id, "moda");
        assert_eq!(
            writes(&actions),
            [&ConfigEdit::Set {
                key: "theme",
                value: "moda".into()
            }]
        );
        let moda = app.theme.agent_colors.unwrap();
        assert!(sent(&actions).contains(&&ClientRequest::SetColors(moda)));
        app.on_key(k(K::Left));
        app.on_key(k(K::Left));
        assert_eq!(app.theme.id, "terminal", "it wraps around");
        app.host_colors = Some(TermColors {
            fg: (1, 1, 1),
            bg: (2, 2, 2),
            ansi: None,
        });
        app.on_key(k(K::Right));
        let actions = app.on_key(k(K::Left));
        assert!(
            sent(&actions).contains(&&ClientRequest::SetColors(app.host_colors.unwrap())),
            "the terminal theme tells agents the host's colours"
        );
    }

    #[test]
    fn with_16_colours_a_painting_theme_is_not_drawn_and_the_screen_says_why() {
        let (mut app, _) = app();
        app.detected_depth = ColorDepth::Ansi16;
        app.on_key(k(K::Char('s')));
        app.on_key(k(K::Right));
        assert_eq!(app.config.theme, "moda");
        assert_eq!(app.theme.id, "terminal");
        assert_eq!(
            top_note(&app).unwrap(),
            "moda needs 256 colours; this terminal has 16"
        );
    }

    #[test]
    fn a_setting_of_the_local_file_is_left_alone() {
        let (mut app, _) = app();
        app.local_settings = vec!["theme".into()];
        app.on_key(k(K::Char('s')));
        assert!(app.on_key(k(K::Right)).is_empty());
        assert_eq!(app.config.theme, "uskudar");
        assert_eq!(top_note(&app).unwrap(), "theme is set in config.local.toml");
    }

    /// Opens the keys screen on the grid's first action, the quick prompt.
    fn keys_screen() -> App {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        for _ in 0..4 {
            app.on_key(k(K::Char('j')));
        }
        app.on_key(k(K::Enter));
        assert!(matches!(app.overlays.last(), Some(Overlay::Keys(_))));
        app
    }

    #[test]
    fn a_key_is_bound_from_the_keys_screen_and_saved() {
        let mut app = keys_screen();
        app.on_key(k(K::Enter));
        let actions = app.on_key(k(K::Char('b')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Keys(_))));
        assert_eq!(
            writes(&actions),
            [&ConfigEdit::Keys {
                table: "grid",
                bindings: vec![
                    ("b".into(), "quick_prompt".into()),
                    ("p".into(), "none".into())
                ],
            }]
        );
        assert_eq!(top_note(&app).unwrap(), "b: new task: prompt, CLI, model");
        app.on_key(k(K::Backspace));
        assert!(
            app.keymap
                .keys(Context::Grid, KeyAction::QuickPrompt)
                .is_empty()
        );
        let actions = app.on_key(k(K::Char('R')));
        assert_eq!(
            app.keymap
                .key(Context::Grid, KeyAction::QuickPrompt)
                .as_deref(),
            Some("p")
        );
        assert_eq!(
            writes(&actions),
            [&ConfigEdit::Keys {
                table: "grid",
                bindings: vec![],
            }]
        );
    }

    #[test]
    fn a_key_another_action_has_is_swapped_only_when_asked() {
        let mut app = keys_screen();
        app.on_key(k(K::Enter));
        assert!(app.on_key(k(K::Char('k'))).is_empty());
        assert!(top_note(&app).unwrap().starts_with("k is \"card above\""));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(writes(&actions).len(), 1);
        assert_eq!(
            app.keymap
                .key(Context::Grid, KeyAction::QuickPrompt)
                .as_deref(),
            Some("k")
        );
        assert_eq!(
            app.keymap.key(Context::Grid, KeyAction::Up).as_deref(),
            Some("p")
        );

        app.on_key(k(K::Enter));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Esc));
        assert!(
            matches!(app.overlays.last(), Some(Overlay::Keys(_))),
            "Esc cancels"
        );
        assert_eq!(
            app.keymap.key(Context::Grid, KeyAction::Down).as_deref(),
            Some("j")
        );
    }

    #[test]
    fn keys_that_cannot_be_bound_are_refused_where_they_are_pressed() {
        let mut app = keys_screen();
        app.on_key(k(K::Enter));
        assert!(app.on_key(ctrl('c')).is_empty());
        assert!(top_note(&app).unwrap().starts_with("C-c always quits"));
        assert!(matches!(app.overlays.last(), Some(Overlay::KeyCapture(_))));
    }

    #[test]
    fn the_prefix_is_set_from_the_settings() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Enter));
        assert!(app.on_key(k(K::Char('x'))).is_empty());
        assert!(
            top_note(&app)
                .unwrap()
                .contains("would no longer reach the session")
        );
        let actions = app.on_key(ctrl(' '));
        assert_eq!(app.keymap.prefix.to_string(), "C-Space");
        assert_eq!(
            writes(&actions),
            [&ConfigEdit::Set {
                key: "prefix",
                value: "C-Space".into()
            }]
        );
        assert!(matches!(app.overlays.last(), Some(Overlay::Settings(_))));
    }

    #[test]
    fn the_pane_setting_is_saved_and_ends_a_prefix_z_choice() {
        let (mut app, _) = app();
        app.pane_override = Some(PanePosition::Right);
        app.on_key(k(K::Char('s')));
        for _ in 0..3 {
            app.on_key(k(K::Char('j')));
        }
        let actions = app.on_key(k(K::Right));
        assert_eq!(app.config.pane_position, PanePosition::Right);
        assert_eq!(app.pane_position(), PanePosition::Right);
        assert_eq!(
            writes(&actions),
            [&ConfigEdit::Set {
                key: "pane_position",
                value: "right".into()
            }]
        );
        app.on_key(k(K::Right));
        assert_eq!(
            app.config.pane_position,
            PanePosition::Left,
            "then left, bottom, top"
        );
    }

    #[test]
    fn a_swap_gives_the_other_action_every_old_key_or_says_it_has_none() {
        let mut app = keys_screen();
        app.on_key(k(K::Enter));
        app.on_key(k(K::Char('k')));
        app.on_key(k(K::Enter));
        assert_eq!(
            top_note(&app).unwrap(),
            "k: new task: prompt, CLI, model · p: card above"
        );
        // An action with no key: the other one is left with none, and says so.
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Backspace));
        app.on_key(k(K::Enter));
        app.on_key(k(K::Char('k')));
        app.on_key(k(K::Enter));
        assert_eq!(
            top_note(&app).unwrap(),
            "k: send the next instruction without entering · new task: prompt, CLI, model has no key now"
        );
    }

    #[test]
    fn a_key_config_toml_cannot_name_is_refused() {
        let mut app = keys_screen();
        app.on_key(k(K::Enter));
        assert!(app.on_key(k(K::CapsLock)).is_empty());
        assert!(top_note(&app).unwrap().starts_with("that key has no name"));
        assert!(
            writes(&app.on_key(k(K::Delete))).len() == 1,
            "Delete has a name"
        );
    }

    #[test]
    fn the_splash_goes_after_a_second_or_at_a_key_and_the_key_is_not_used() {
        let (mut app, _) = app();
        app.screen = ratatui::layout::Rect::new(0, 0, 120, 40);
        let t0 = Instant::now();
        app.start_splash(t0);
        assert!(app.showing.is_some());
        app.tick(t0 + Duration::from_millis(500));
        assert!(app.showing.is_some());
        assert_eq!(
            app.next_wake(t0).unwrap(),
            t0 + Duration::from_millis(100),
            "it moves"
        );
        app.tick(t0 + Duration::from_secs(1));
        assert_eq!(app.showing, None);

        app.start_splash(Instant::now());
        assert!(app.on_key(k(K::Char('p'))).is_empty());
        assert!(app.showing.is_none());
        assert!(app.overlays.is_empty(), "the key only took the splash away");

        app.config.scenes.splash = false;
        app.start_splash(Instant::now());
        assert!(app.showing.is_none());
    }

    #[test]
    fn after_the_idle_minutes_a_scene_comes_and_a_waiting_agent_ends_it() {
        let (mut app, s) = app();
        app.config.scenes.idle_minutes = 10;
        let now = Instant::now();
        app.last_input = now - Duration::from_secs(9 * 60);
        app.tick(now);
        assert!(app.showing.is_none());
        assert_eq!(
            app.next_wake(now),
            Some(app.last_input + Duration::from_secs(600))
        );
        app.last_input = now - Duration::from_secs(10 * 60);
        app.tick(now);
        let shown = app.showing.unwrap();
        assert_eq!(shown.kind, ShowKind::Idle);
        assert!(app.scenes.contains_key(shown.name));

        let mut waiting = s[1].clone();
        waiting.status = AgentStatus::NeedsFeedback;
        app.on_event(ServerEvent::SessionUpdated(waiting));
        assert!(app.showing.is_none(), "a red agent ends the idle screen");
        assert_eq!(app.selected, Some(s[1].id), "on its card");
    }

    #[test]
    fn no_idle_screen_when_it_is_off_or_the_terminal_has_16_colours() {
        let (mut app, _) = app();
        let now = Instant::now();
        app.last_input = now - Duration::from_secs(3600);
        app.config.scenes.idle_minutes = 0;
        app.tick(now);
        assert!(app.showing.is_none());
        app.config.scenes.idle_minutes = 10;
        app.theme = Theme::named("moda", ColorDepth::Ansi16);
        app.tick(now);
        assert!(app.showing.is_none());
    }

    #[test]
    fn scenes_come_from_the_pool_and_never_twice_in_a_row() {
        let (mut app, _) = app();
        app.config.scenes.pool = vec!["galata".into(), "vapur".into()];
        let mut last = app.pick_scene();
        for _ in 0..20 {
            app.scene = last;
            let next = app.pick_scene();
            assert!(["galata", "vapur"].contains(&next));
            assert_ne!(next, last);
            last = next;
        }
        app.config.scenes.pool = vec!["galata".into()];
        app.scene = "galata";
        assert_eq!(app.pick_scene(), "galata", "a pool of one repeats");
    }

    #[test]
    fn without_animations_the_scene_stands_still() {
        let (mut app, _) = app();
        app.config.animations = false;
        let t0 = Instant::now();
        app.start_splash(t0);
        assert_eq!(app.scene_frame(t0 + Duration::from_secs(3)), 0);
        assert_eq!(
            app.next_wake(t0),
            Some(t0 + Duration::from_secs(1)),
            "only the splash's end"
        );
    }

    fn alerts(actions: &[Action]) -> Vec<&Alert> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Alert(alert) => Some(alert),
                _ => None,
            })
            .collect()
    }

    fn update(app: &mut App, info: &SessionInfo, status: AgentStatus) -> Vec<Action> {
        let mut info = info.clone();
        info.status = status;
        app.on_event(ServerEvent::SessionUpdated(info))
    }

    #[test]
    fn a_card_that_starts_waiting_or_finishes_is_announced() {
        let (mut app, s) = app();
        let waiting = update(&mut app, &s[1], AgentStatus::NeedsFeedback);
        assert_eq!(
            alerts(&waiting),
            [&Alert {
                text: format!("{} · api waits for you", s[1].display_name()),
                waiting: true,
            }]
        );
        assert!(
            alerts(&update(&mut app, &s[1], AgentStatus::NeedsFeedback)).is_empty(),
            "once"
        );
        let done = update(&mut app, &s[1], AgentStatus::Unseen);
        assert!(alerts(&done)[0].text.ends_with("is done"));
        assert!(!alerts(&done)[0].waiting);
        assert!(alerts(&update(&mut app, &s[1], AgentStatus::Running)).is_empty());
    }

    #[test]
    fn the_card_you_are_typing_into_is_not_announced_unless_you_are_away() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        let id = app.selected.unwrap();
        let focused = s.iter().find(|x| x.id == id).unwrap().clone();
        assert!(alerts(&update(&mut app, &focused, AgentStatus::NeedsFeedback)).is_empty());
        update(&mut app, &focused, AgentStatus::Running);
        app.window_focused = false;
        assert_eq!(
            alerts(&update(&mut app, &focused, AgentStatus::NeedsFeedback)).len(),
            1
        );
    }

    #[test]
    fn archived_cards_and_the_first_state_are_quiet() {
        let (mut app, s) = app();
        let mut archived = s[2].clone();
        archived.archived = true;
        archived.status = AgentStatus::NeedsFeedback;
        assert!(alerts(&app.on_event(ServerEvent::SessionUpdated(archived))).is_empty());
        let mut state = app.state.clone();
        for x in &mut state.sessions {
            x.status = AgentStatus::Unseen;
        }
        assert!(alerts(&app.on_event(ServerEvent::State(state))).is_empty());
    }

    #[test]
    fn sounds_notifications_and_scenes_are_set_from_the_settings() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        for _ in 0..5 {
            app.on_key(k(K::Char('j')));
        }
        let edits: Vec<ConfigEdit> = [
            app.on_key(k(K::Right)),
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Left))
            },
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Enter))
            },
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Right))
            },
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Right))
            },
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Left))
            },
            {
                app.on_key(k(K::Char('j')));
                app.on_key(k(K::Right))
            },
        ]
        .into_iter()
        .flat_map(|actions| writes(&actions).into_iter().cloned().collect::<Vec<_>>())
        .collect();
        assert_eq!(
            edits,
            [
                ConfigEdit::Set {
                    key: "notify.done_sound",
                    value: "marti".into()
                },
                ConfigEdit::Set {
                    key: "notify.waiting_sound",
                    value: "bell".into()
                },
                ConfigEdit::SetBool {
                    key: "notify.desktop",
                    value: false
                },
                ConfigEdit::SetBool {
                    key: "notify.toasts",
                    value: false
                },
                ConfigEdit::SetBool {
                    key: "scenes.splash",
                    value: false
                },
                ConfigEdit::SetInt {
                    key: "scenes.idle_minutes",
                    value: 5
                },
                ConfigEdit::SetBool {
                    key: "animations",
                    value: false
                },
            ]
        );
        assert_eq!(app.config.notify.done_sound, Sound::Marti);
        assert_eq!(app.config.notify.waiting_sound, Sound::Bell);
        assert!(!app.config.animations);
        app.local_settings = vec!["scenes.idle_minutes".into()];
        app.on_key(k(K::Char('k')));
        assert!(
            app.on_key(k(K::Right)).is_empty(),
            "set in config.local.toml"
        );
        app.on_key(k(K::Char('k')));
        assert_eq!(app.on_key(k(K::Right)).len(), 1, "the splash is not");
    }

    #[test]
    fn a_sound_chosen_in_the_settings_is_heard_once() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        for _ in 0..6 {
            app.on_key(k(K::Char('j')));
        }
        let actions = app.on_key(k(K::Left));
        assert!(matches!(
            actions[..],
            [Action::WriteConfig(_), Action::Preview(Sound::Bell)]
        ));
        app.on_key(k(K::Char('k')));
        let actions = app.on_key(k(K::Right));
        assert!(matches!(
            actions[..],
            [Action::WriteConfig(_), Action::Preview(Sound::Marti)]
        ));
    }

    #[test]
    fn the_old_sounds_setting_in_the_local_file_holds_both_sounds() {
        let (mut app, _) = app();
        app.local_settings = vec!["notify.sounds".into()];
        app.on_key(k(K::Char('s')));
        for _ in 0..5 {
            app.on_key(k(K::Char('j')));
        }
        assert!(app.on_key(k(K::Right)).is_empty());
        assert_eq!(
            top_note(&app).unwrap(),
            "notify.sounds is set in config.local.toml"
        );
        app.on_key(k(K::Char('j')));
        assert!(app.on_key(k(K::Right)).is_empty());
        app.on_key(k(K::Char('j')));
        assert_eq!(app.on_key(k(K::Right)).len(), 1, "desktop is not");
    }

    #[test]
    fn a_scene_moves_only_where_it_fits() {
        let (mut app, _) = app();
        let t0 = Instant::now();
        app.start_splash(t0);
        app.screen = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert_eq!(
            app.next_wake(t0),
            Some(t0 + SPLASH),
            "the wordmark stands still"
        );
        app.screen = ratatui::layout::Rect::new(0, 0, 110, 30);
        assert_eq!(app.next_wake(t0), Some(t0 + Duration::from_millis(100)));
    }

    #[test]
    fn behind_the_idle_screen_nothing_is_seen_and_everything_is_announced() {
        let (mut app, s) = app();
        app.on_key(k(K::Enter));
        let id = app.selected.unwrap();
        let focused = s.iter().find(|x| x.id == id).unwrap().clone();
        app.showing = Some(Showing {
            name: "galata",
            since: Instant::now(),
            kind: ShowKind::Idle,
        });
        let actions = update(&mut app, &focused, AgentStatus::Unseen);
        assert!(
            !sent(&actions)
                .iter()
                .any(|r| matches!(r, ClientRequest::MarkSeen { .. })),
            "not seen"
        );
        assert_eq!(alerts(&actions).len(), 1, "and announced");
        let actions = update(&mut app, &focused, AgentStatus::NeedsFeedback);
        assert_eq!(alerts(&actions).len(), 1);
        assert!(app.showing.is_none());
    }

    #[test]
    fn every_scene_can_come_up_first_galata_too() {
        let mut seen = std::collections::HashSet::new();
        for seed in 1..200u64 {
            let mut app = App::new();
            app.rng = seed;
            app.scene = "";
            seen.insert(app.pick_scene());
        }
        assert!(seen.contains("galata"), "{seen:?}");
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn a_waiting_card_the_grid_does_not_show_is_not_selected() {
        let (mut app, s) = app();
        app.showing = Some(Showing {
            name: "galata",
            since: Instant::now(),
            kind: ShowKind::Idle,
        });
        app.view = View::Archive;
        update(&mut app, &s[0], AgentStatus::NeedsFeedback);
        assert!(app.showing.is_none());
        assert_ne!(
            app.selected,
            Some(s[0].id),
            "the archive view shows other cards"
        );
    }

    #[test]
    fn the_toasts_row_turns_agent_toasts_off_and_saves_it() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        let row = SETTING_ROWS
            .iter()
            .position(|r| *r == SettingRow::Toasts)
            .unwrap();
        for _ in 0..row {
            app.on_key(k(K::Down));
        }
        let actions = app.on_key(k(K::Right));
        assert!(!app.config.notify.toasts);
        assert!(actions.contains(&Action::WriteConfig(ConfigEdit::SetBool {
            key: "notify.toasts",
            value: false,
        })));
    }

    #[test]
    fn the_diff_layout_setting_flips_and_saves() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        let row = SETTING_ROWS
            .iter()
            .position(|r| *r == SettingRow::DiffLayout)
            .unwrap();
        if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
            v.row = row;
        }
        let actions = app.on_key(k(K::Right));
        assert_eq!(
            app.config.diff.layout,
            termist_core::config::DiffLayout::Split
        );
        assert_eq!(
            actions,
            [Action::WriteConfig(ConfigEdit::Set {
                key: "diff.layout",
                value: "split".into()
            })]
        );
    }

    #[test]
    fn the_agents_row_stops_telling_agents_of_termist_and_saves_it() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        let row = SETTING_ROWS
            .iter()
            .position(|r| *r == SettingRow::TeachAgents)
            .unwrap();
        if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
            v.row = row;
        }
        let actions = app.on_key(k(K::Right));
        assert!(!app.config.agents.teach);
        assert_eq!(
            actions,
            [Action::WriteConfig(ConfigEdit::SetBool {
                key: "agents.teach",
                value: false
            })],
            "read by the daemon when an agent starts: nothing to send"
        );
    }

    #[test]
    fn the_pull_requests_setting_tells_the_daemon() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        let row = SETTING_ROWS
            .iter()
            .position(|r| *r == SettingRow::PullRequests)
            .unwrap();
        if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
            v.row = row;
        }
        let actions = app.on_key(k(K::Right));
        assert!(!app.config.github.enabled);
        assert!(actions.contains(&Action::WriteConfig(ConfigEdit::SetBool {
            key: "github.enabled",
            value: false
        })));
        assert!(sent(&actions).contains(&&ClientRequest::SetGitHub { enabled: false }));
    }

    #[test]
    fn the_status_rows_turn_each_part_off_and_save_it() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('s')));
        for (row, key) in [
            (SettingRow::StatusCpu, "status.cpu"),
            (SettingRow::StatusRam, "status.ram"),
            (SettingRow::StatusBattery, "status.battery"),
            (SettingRow::StatusClock, "status.clock"),
        ] {
            let at = SETTING_ROWS.iter().position(|r| *r == row).unwrap();
            if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
                v.row = at;
            }
            let actions = app.on_key(k(K::Right));
            assert!(
                actions.contains(&Action::WriteConfig(ConfigEdit::SetBool {
                    key,
                    value: false
                })),
                "{key}"
            );
        }
        assert!(!app.config.status.any());
    }

    use crate::prs::fixtures::{project_prs, repo, summary};

    fn with_prs(app: &mut App, project: ProjectId) {
        let data = project_prs(vec![repo(
            1,
            "site",
            vec![
                summary(212, "Add a dealer filter", "bob"),
                summary(209, "Fix lazy images", "alice"),
            ],
        )]);
        app.on_event(ServerEvent::Prs {
            project,
            state: GhState::Ok,
            discovered: data.discovered,
            repos: data.repos,
        });
    }

    fn focus(actions: &[Action]) -> Vec<(Option<ProjectId>, Option<u32>)> {
        sent(actions)
            .into_iter()
            .filter_map(|r| match r {
                ClientRequest::SetPrFocus { project, pr, .. } => {
                    Some((*project, pr.map(|p| p.number)))
                }
                _ => None,
            })
            .collect()
    }

    /// What the daemon is told of the issues: (project, issues looked at).
    fn issue_focus(actions: &[Action]) -> Vec<(Option<ProjectId>, bool)> {
        sent(actions)
            .into_iter()
            .filter_map(|r| match r {
                ClientRequest::SetPrFocus {
                    project, issues, ..
                } => Some((*project, *issues)),
                _ => None,
            })
            .collect()
    }

    fn with_issues(app: &mut App, project: ProjectId) {
        let data = crate::prs::issues::tests::data();
        app.on_event(ServerEvent::Issues {
            project,
            state: GhState::Ok,
            repos: data.repos,
        });
    }

    #[test]
    fn i_opens_the_issues_and_the_daemon_reads_them() {
        let (mut app, s) = app();
        let api = s[0].project;
        let actions = app.on_key(k(K::Char('i')));
        assert!(matches!(&app.view, View::Prs(v) if v.section == Section::Issues));
        assert_eq!(issue_focus(&actions), [(Some(api), true)]);
        // `v` there: the pull requests, no longer the issues.
        let actions = app.on_key(k(K::Char('v')));
        assert!(matches!(&app.view, View::Prs(v) if v.section == Section::Pulls));
        assert_eq!(issue_focus(&actions), [(Some(api), false)]);
        app.on_key(k(K::Char('i')));
        let actions = app.on_key(k(K::Char('i')));
        assert_eq!(app.view, View::Grid, "i again: back to the grid");
        assert_eq!(issue_focus(&actions), [(None, false)]);
    }

    #[test]
    fn tab_goes_between_the_pull_requests_and_the_issues() {
        let (mut app, s) = app();
        let api = s[0].project;
        with_prs(&mut app, api);
        with_issues(&mut app, api);
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Tab));
        assert_eq!(issue_focus(&actions), [(Some(api), true)]);
        let actions = app.on_key(k(K::Char('b')));
        assert_eq!(
            actions,
            [Action::OpenUrl(
                "https://github.com/acme/site/issues/123".into()
            )]
        );
        app.on_key(k(K::Tab));
        assert!(matches!(&app.view, View::Prs(v) if v.section == Section::Pulls));
        // A pull request open: Tab is its own (its tabs), and the issues are not read.
        app.on_key(k(K::Enter));
        app.on_key(k(K::Tab));
        assert!(matches!(&app.view, View::Prs(v) if v.section == Section::Pulls));
        let actions = app.on_key(k(K::Char('i')));
        assert_eq!(
            focus(&actions),
            [(Some(api), None)],
            "the pull request behind the issues is not read"
        );
    }

    #[test]
    fn enter_on_an_issue_starts_a_task_on_it_in_a_worktree_named_after_it() {
        let (mut app, s) = app();
        let api = s[0].project;
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Claude,
            available: true,
        }]));
        with_prs(&mut app, api);
        with_issues(&mut app, api);
        app.on_key(k(K::Char('i')));
        app.on_key(k(K::Enter));
        let Some(Overlay::QuickPrompt(q)) = app.overlays.last() else {
            panic!("the quick prompt")
        };
        assert_eq!(
            q.input.text(),
            "Work on acme/site#123: Login redirect loses the query\n\nIt breaks.\n\nhttps://github.com/acme/site/issues/123"
        );
        assert_eq!(
            q.title_from().as_deref(),
            Some("Login redirect loses the query")
        );
        assert_eq!(
            app.new_branch(q).as_deref(),
            Some("123-login-redirect-loses-the"),
            "named after the issue, not the prompt"
        );
        // ^N off and on again: still the issue's.
        app.on_key(ctrl('n'));
        assert_eq!(quick(&app).new_worktree, None);
        app.on_key(ctrl('n'));
        let q = quick(&app);
        assert_eq!(
            app.new_branch(q).as_deref(),
            Some("123-login-redirect-loses-the")
        );
        let actions = app.on_key(k(K::Enter));
        let ticket = sent(&actions)
            .iter()
            .find_map(|r| match r {
                ClientRequest::CreateWorktree { ticket, branch, .. } => {
                    assert_eq!(branch, "123-login-redirect-loses-the");
                    Some(*ticket)
                }
                _ => None,
            })
            .expect("a worktree first");
        let actions = app.on_event(ServerEvent::WorktreeMade {
            ticket,
            path: "/w/site-worktrees/123-login-redirect-loses-the".into(),
            branch: "123-login-redirect-loses-the".into(),
            note: None,
        });
        assert!(sent(&actions).iter().any(|r| matches!(
            r,
            ClientRequest::CreateSession { issue: Some(url), title_from: Some(t), .. }
                if url == "https://github.com/acme/site/issues/123"
                    && t == "Login redirect loses the query"
        )));
        assert_eq!(app.prompt_draft, None, "an issue's words are no draft");
    }

    #[test]
    fn an_issue_task_closed_leaves_no_draft_and_without_a_worktree_starts_in_the_folder() {
        let (mut app, s) = app();
        let api = s[0].project;
        app.on_event(ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::Claude,
            available: true,
        }]));
        with_prs(&mut app, api);
        with_issues(&mut app, api);
        app.on_key(k(K::Char('i')));
        app.on_key(k(K::Enter));
        app.on_key(k(K::Esc));
        assert_eq!(app.prompt_draft, None);
        app.on_key(k(K::Enter));
        app.on_key(ctrl('n'));
        let actions = app.on_key(k(K::Enter));
        assert!(sent(&actions).iter().any(|r| matches!(
            r,
            ClientRequest::CreateSession {
                cwd: None,
                issue: Some(_),
                ..
            }
        )));
    }

    #[test]
    fn v_opens_the_pull_requests_and_tells_the_daemon() {
        let (mut app, s) = app();
        let api = s[0].project;
        let actions = app.on_key(k(K::Char('v')));
        assert!(matches!(app.view, View::Prs(_)));
        assert_eq!(focus(&actions), [(Some(api), None)]);
        let actions = app.on_key(k(K::Char('v')));
        assert_eq!(app.view, View::Grid);
        assert_eq!(focus(&actions), [(None, None)]);
    }

    #[test]
    fn enter_opens_a_pull_request_and_marks_it_seen() {
        let (mut app, s) = app();
        let api = s[0].project;
        with_prs(&mut app, api);
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Enter));
        assert!(sent(&actions).iter().any(|r| matches!(
            r,
            ClientRequest::MarkPrSeen { pr, .. } if pr.number == 212
        )));
        assert_eq!(focus(&actions), [(Some(api), Some(212))]);
        let actions = app.on_key(k(K::Esc));
        assert_eq!(focus(&actions), [(Some(api), None)]);
    }

    #[test]
    fn another_tab_starts_the_view_afresh() {
        let (mut app, s) = app();
        let web = s[3].project;
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Char(']')));
        assert_eq!(focus(&actions), [(Some(web), None)]);
        assert!(matches!(&app.view, View::Prs(v) if v.project == Some(web)));
    }

    #[test]
    fn with_github_off_the_daemon_is_not_told() {
        let (mut app, _) = app();
        app.config.github.enabled = false;
        let actions = app.on_key(k(K::Char('v')));
        assert!(focus(&actions).is_empty());
        assert!(matches!(app.view, View::Prs(_)), "the view says why");
    }

    #[test]
    fn shift_r_reads_github_again() {
        let (mut app, s) = app();
        let actions = app.on_key(KeyEvent::new(K::Char('R'), M::SHIFT));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::RefreshPrs {
                project: s[0].project
            }]
        );
    }

    /// The diff flags of the focuses among `actions`.
    fn diff_focus(actions: &[Action]) -> Vec<bool> {
        sent(actions)
            .into_iter()
            .filter_map(|r| match r {
                ClientRequest::SetPrFocus { diff, .. } => Some(*diff),
                _ => None,
            })
            .collect()
    }

    fn pr_212() -> PrRef {
        PrRef {
            repo: termist_core::github::RepoId(1),
            number: 212,
        }
    }

    /// Two files of 212: `src/a.rs` already viewed, `src/b.rs` not.
    fn diff_of_212() -> PrDiff {
        use crate::diff::tree::tests::file;
        let mut files = vec![file("src/a.rs"), file("src/b.rs")];
        files[0].viewed = termist_core::github::Viewed::Viewed;
        PrDiff {
            head_oid: "h1".into(),
            files,
            more: 0,
        }
    }

    /// The open diff of the PR view.
    fn open_diff(app: &App) -> Option<&crate::diff::DiffView> {
        match &app.view {
            View::Prs(v) => v.detail.as_ref()?.diff.as_ref(),
            _ => None,
        }
    }

    #[test]
    fn d_opens_the_diff_and_esc_comes_back_to_the_files() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Char('d')));
        assert_eq!(diff_focus(&actions), [true], "from the inbox, at once");
        assert!(
            sent(&actions)
                .iter()
                .any(|r| matches!(r, ClientRequest::MarkPrSeen { .. }))
        );
        let actions = app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        assert!(diff_focus(&actions).is_empty());
        assert_eq!(
            open_diff(&app).and_then(|d| d.file.as_deref()),
            Some("src/b.rs"),
            "the first file not viewed"
        );
        let actions = app.on_key(k(K::Esc));
        assert_eq!(diff_focus(&actions), [false]);
        let View::Prs(v) = &app.view else { panic!() };
        let d = v.detail.as_ref().unwrap();
        assert_eq!(
            (d.tab, d.file.as_deref()),
            (crate::prs::Tab::Files, Some("src/b.rs"))
        );
        let actions = app.on_key(k(K::Enter));
        assert_eq!(diff_focus(&actions), [true]);
        assert_eq!(
            open_diff(&app).and_then(|d| d.file.as_deref()),
            Some("src/b.rs"),
            "Enter on the Files tab opens its file; the diff read is used at once"
        );
    }

    #[test]
    fn ctrl_r_asks_github_and_a_refusal_undoes_it() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        let actions = app.on_key(ctrl('r'));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetFileViewed {
                pr: pr_212(),
                path: "src/b.rs".into(),
                viewed: true
            }]
        );
        assert!(!open_diff(&app).unwrap().pending.is_empty());
        app.on_event(ServerEvent::PrWriteFailed {
            pr: pr_212(),
            ticket: None,
            message: "couldn't mark b.rs viewed · no access".into(),
        });
        assert!(open_diff(&app).unwrap().pending.is_empty());
        assert_eq!(
            app.toasts.items().next().map(|t| t.text.as_str()),
            Some("✗ couldn't mark b.rs viewed · no access")
        );
    }

    #[test]
    fn s_in_the_diff_flips_the_layout_and_saves_it() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        let actions = app.on_key(k(K::Char('s')));
        assert_eq!(
            app.config.diff.layout,
            termist_core::config::DiffLayout::Split
        );
        assert_eq!(
            actions,
            [Action::WriteConfig(ConfigEdit::Set {
                key: "diff.layout",
                value: "split".into()
            })]
        );
    }

    #[test]
    fn a_failed_read_without_a_diff_keeps_the_one_shown() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Failed("HTTP 502".into()),
            diff: None,
        });
        let (state, diff) = &app.pr_diffs[&pr_212()];
        assert_eq!(*state, GhState::Failed("HTTP 502".into()));
        assert!(diff.is_some(), "the failure is told, the diff stays");
    }

    #[test]
    fn only_the_open_diff_is_kept() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        app.on_key(k(K::Esc));
        app.on_key(k(K::Esc));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Char('d')));
        let pr_209 = PrRef {
            number: 209,
            ..pr_212()
        };
        app.on_event(ServerEvent::PrDiff {
            pr: pr_209,
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        assert_eq!(
            app.pr_diffs.keys().collect::<Vec<_>>(),
            [&pr_209],
            "a diff of up to 8 MB is not kept once left; the daemon sends it again"
        );
    }

    #[test]
    fn the_diff_search_takes_every_letter() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('d')));
        app.on_event(ServerEvent::PrDiff {
            pr: pr_212(),
            state: GhState::Ok,
            diff: Some(Box::new(diff_of_212())),
        });
        app.on_key(k(K::Char('/')));
        for c in "qv]s?".chars() {
            app.on_key(k(K::Char(c)));
        }
        assert_eq!(open_diff(&app).map(|d| d.query.as_str()), Some("qv]s?"));
        assert!(app.overlays.is_empty() && app.mode == Mode::Grid);
    }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(k(K::Char(c)));
        }
    }

    fn top_compose(app: &App) -> Option<&Compose> {
        match app.overlays.last() {
            Some(Overlay::Compose(c)) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn a_comment_goes_under_a_ticket_and_the_box_closes_once_written() {
        let (mut app, _) = app();
        app.open_compose(Compose::new(pr_212(), Target::Comment, ""));
        typed(&mut app, "lgtm");
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::WritePr {
                pr: pr_212(),
                ticket: 1,
                write: termist_core::github::PrWrite::Comment {
                    body: "lgtm".into()
                },
            }]
        );
        assert_eq!(
            top_compose(&app).map(|c| c.state.clone()),
            Some(Sending::Sending(1))
        );
        app.on_event(ServerEvent::PrWritten {
            pr: pr_212(),
            ticket: 1,
        });
        assert!(top_compose(&app).is_none(), "closed");
        assert!(app.drafts.is_empty(), "nothing left to keep");
    }

    #[test]
    fn a_refusal_keeps_the_box_and_its_words_and_says_why() {
        let (mut app, _) = app();
        app.open_compose(Compose::new(pr_212(), Target::Submit, ""));
        app.on_key(k(K::Tab));
        app.on_key(k(K::Enter));
        app.on_event(ServerEvent::PrWriteFailed {
            pr: pr_212(),
            ticket: Some(1),
            message: "couldn't send your review · Review Can not approve your own pull request"
                .into(),
        });
        let c = top_compose(&app).unwrap();
        assert_eq!(
            c.state,
            Sending::Failed("Review Can not approve your own pull request".into())
        );
        assert_eq!(app.toasts.items().count(), 0, "the box says it");
    }

    #[test]
    fn esc_keeps_a_draft_for_the_same_target_and_a_late_refusal_is_a_toast() {
        let (mut app, _) = app();
        app.open_compose(Compose::new(pr_212(), Target::Comment, ""));
        typed(&mut app, "half a thought");
        app.on_key(k(K::Esc));
        assert!(top_compose(&app).is_none());
        app.open_compose(Compose::new(pr_212(), Target::Comment, ""));
        assert_eq!(top_compose(&app).unwrap().input.text(), "half a thought");
        app.on_key(k(K::Enter));
        app.on_key(k(K::Esc));
        assert!(
            top_compose(&app).is_none(),
            "Esc closes even while it is on its way"
        );
        app.on_event(ServerEvent::PrWriteFailed {
            pr: pr_212(),
            ticket: Some(1),
            message: "couldn't post your comment · no access".into(),
        });
        assert_eq!(
            app.toasts.items().next().map(|t| t.text.as_str()),
            Some("✗ couldn't post your comment · no access")
        );
        assert_eq!(
            app.drafts
                .get(&(pr_212(), Target::Comment))
                .map(String::as_str),
            Some("half a thought"),
            "the words wait for another try"
        );
    }

    #[test]
    fn a_box_reopened_while_its_words_are_on_their_way_does_not_send_them_twice() {
        let (mut app, _) = app();
        app.open_compose(Compose::new(pr_212(), Target::Comment, ""));
        typed(&mut app, "lgtm");
        app.on_key(k(K::Enter));
        app.on_key(k(K::Esc));
        app.open_compose(Compose::new(pr_212(), Target::Comment, ""));
        assert_eq!(
            top_compose(&app).map(|c| c.state.clone()),
            Some(Sending::Sending(1)),
            "it says it is still on its way"
        );
        assert!(sent(&app.on_key(k(K::Enter))).is_empty(), "no second post");
        app.on_event(ServerEvent::PrWritten {
            pr: pr_212(),
            ticket: 1,
        });
        assert!(top_compose(&app).is_none(), "the answer closes it");
    }

    #[test]
    fn details_from_the_daemon_are_kept() {
        let (mut app, _) = app();
        let pr = PrRef {
            repo: termist_core::github::RepoId(1),
            number: 212,
        };
        app.on_event(ServerEvent::PrDetail {
            pr,
            state: GhState::Failed("HTTP 502".into()),
            detail: None,
        });
        assert_eq!(app.pr_details[&pr].0, GhState::Failed("HTTP 502".into()));
    }

    #[test]
    fn turning_github_on_in_the_settings_tells_the_daemon_what_is_looked_at() {
        let (mut app, s) = app();
        app.config.github.enabled = false;
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('s')));
        let row = SETTING_ROWS
            .iter()
            .position(|r| *r == SettingRow::PullRequests)
            .unwrap();
        if let Some(Overlay::Settings(v)) = app.overlays.last_mut() {
            v.row = row;
        }
        let actions = app.on_key(k(K::Right));
        assert!(app.config.github.enabled);
        assert_eq!(focus(&actions), [(Some(s[0].project), None)]);
    }

    #[test]
    fn a_toast_click_out_of_the_pull_requests_tells_the_daemon() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('v')));
        let at = waiting_toast(&mut app, &s);
        let actions = click(&mut app, at);
        assert_eq!(app.view, View::Grid);
        assert_eq!(focus(&actions), [(None, None)]);
    }

    #[test]
    fn the_search_takes_every_key_in_the_pull_requests() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('/')));
        let mut actions = vec![];
        for c in ['v', '1', 'q', ']'] {
            actions.extend(app.on_key(k(K::Char(c))));
        }
        assert!(matches!(&app.view, View::Prs(v) if v.query == "v1q]" && v.typing));
        assert_eq!(app.project, Some(s[0].project));
        assert_eq!(app.mode, Mode::Grid);
        assert!(sent(&actions).is_empty(), "{actions:?}");
    }

    fn repos_event(project: ProjectId) -> ServerEvent {
        use termist_core::github::{RepoId, RepoInfo};
        let info = |id: i64, name: &str, visible: bool, count: Option<u32>| RepoInfo {
            id: RepoId(id),
            name: name.into(),
            slug: format!("acme/{name}"),
            visible,
            account: Some("work".into()),
            pinned: false,
            open_count: count,
            state: GhState::Ok,
        };
        ServerEvent::Repos {
            project,
            accounts: vec!["work".into(), "me".into()],
            repos: vec![
                info(1, "admin", true, Some(2)),
                info(2, "site", false, Some(4)),
            ],
        }
    }

    #[test]
    fn m_opens_the_repos_and_space_shows_or_hides_one() {
        let (mut app, s) = app();
        let api = s[0].project;
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Char('m')));
        assert!(matches!(app.overlays.last(), Some(Overlay::Repos { .. })));
        assert!(sent(&actions).contains(&&ClientRequest::ListRepos { project: api }));
        app.on_event(repos_event(api));
        let actions = app.on_key(k(K::Char(' ')));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetRepoVisible {
                repo: termist_core::github::RepoId(1),
                visible: false
            }]
        );
        let Some(Overlay::Repos { picker, .. }) = app.overlays.last() else {
            panic!()
        };
        assert!(!picker.items()[0].visible, "shown at once");
    }

    #[test]
    fn a_chooses_the_account_a_repo_is_read_with() {
        let (mut app, s) = app();
        let api = s[0].project;
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('m')));
        app.on_event(repos_event(api));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Char('a')));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::RepoAccount { .. })
        ));
        app.on_key(k(K::Char('j')));
        app.on_key(k(K::Char('j')));
        let actions = app.on_key(k(K::Enter));
        assert_eq!(
            sent(&actions),
            [&ClientRequest::SetRepoAccount {
                repo: termist_core::github::RepoId(2),
                account: Some("me".into())
            }]
        );
        assert!(
            matches!(app.overlays.last(), Some(Overlay::Repos { .. })),
            "back to the repos"
        );
    }

    #[test]
    fn b_opens_the_selected_pull_request_in_the_browser() {
        let (mut app, s) = app();
        with_prs(&mut app, s[0].project);
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Char('b')));
        assert!(actions.contains(&Action::OpenUrl(
            "https://github.com/acme/site/pull/212".into()
        )));
    }

    #[test]
    fn what_an_agent_did_through_termist_shows_as_a_toast_unless_toasts_are_off() {
        let (mut app, _) = app();
        app.on_event(ServerEvent::Notice {
            text: "Fix Login started Write Tests".into(),
        });
        assert_eq!(
            app.toasts
                .items()
                .map(|t| t.text.clone())
                .collect::<Vec<_>>(),
            ["↳ Fix Login started Write Tests"]
        );
        let (mut quiet, _) = self::app();
        quiet.config.notify.toasts = false;
        quiet.on_event(ServerEvent::Notice {
            text: "Fix Login moved to fix-login".into(),
        });
        assert_eq!(quiet.toasts.items().count(), 0);
    }

    #[test]
    fn a_review_request_shows_a_toast_and_a_quiet_notification() {
        let (mut app, s) = app();
        let pr = PrRef {
            repo: termist_core::github::RepoId(1),
            number: 215,
        };
        let actions = app.on_event(ServerEvent::ReviewRequested {
            project: s[0].project,
            pr,
            repo: "site".into(),
            title: "Bulk edit".into(),
        });
        assert_eq!(
            app.toasts
                .items()
                .map(|t| t.text.clone())
                .collect::<Vec<_>>(),
            ["⇄ #215 · site wants your review"]
        );
        assert!(actions.contains(&Action::Notify(
            "site #215 wants your review: Bulk edit".into()
        )));
        assert!(alerts(&actions).is_empty(), "no sound");
    }

    #[test]
    fn clicking_the_review_toast_opens_the_pull_request() {
        let (mut app, s) = app();
        let api = s[0].project;
        with_prs(&mut app, api);
        app.screen = Rect::new(0, 0, 100, 40);
        let pr = PrRef {
            repo: termist_core::github::RepoId(1),
            number: 209,
        };
        app.on_event(ServerEvent::ReviewRequested {
            project: api,
            pr,
            repo: "site".into(),
            title: "Fix lazy images".into(),
        });
        let r = app.toasts.rects(app.screen)[0];
        use ratatui::crossterm::event::MouseButton::Left;
        let actions = mouse(&mut app, MouseEventKind::Down(Left), r.x + 1, r.y + 1);
        let View::Prs(v) = &app.view else {
            panic!("the PR view opens")
        };
        assert_eq!(v.detail.as_ref().map(|d| d.pr), Some(pr));
        assert!(
            sent(&actions)
                .iter()
                .any(|r| matches!(r, ClientRequest::MarkPrSeen { .. }))
        );
    }

    #[test]
    fn the_review_toast_ends_scroll_back_first() {
        let (mut app, s) = app();
        let api = s[0].project;
        with_prs(&mut app, api);
        app.screen = Rect::new(0, 0, 100, 40);
        history(&mut app, 0, 500);
        app.on_key(k(K::PageUp));
        assert!(app.scrolling);
        let pr = PrRef {
            repo: termist_core::github::RepoId(1),
            number: 209,
        };
        app.on_event(ServerEvent::ReviewRequested {
            project: api,
            pr,
            repo: "site".into(),
            title: "Fix lazy images".into(),
        });
        let r = app.toasts.rects(app.screen)[0];
        use ratatui::crossterm::event::MouseButton::Left;
        let actions = mouse(&mut app, MouseEventKind::Down(Left), r.x + 1, r.y + 1);
        assert!(!app.scrolling);
        assert_eq!(scrolls(&actions), vec![Scroll::Bottom]);
        assert!(matches!(&app.view, View::Prs(v) if v.selected == Some(pr)));
    }

    #[test]
    fn the_repos_window_says_so_when_pull_requests_are_off() {
        let (mut app, _) = app();
        app.config.github.enabled = false;
        app.on_key(k(K::Char('v')));
        let actions = app.on_key(k(K::Char('m')));
        assert!(actions.is_empty());
        assert!(app.overlays.is_empty());
        assert_eq!(app.message.as_deref(), Some("pull requests are off"));
    }

    #[test]
    fn repos_of_another_project_leave_the_open_window_alone() {
        let (mut app, s) = app();
        let api = s[0].project;
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('m')));
        app.on_event(repos_event(api));
        app.on_event(repos_event(ProjectId::new()));
        let Some(Overlay::Repos { picker, .. }) = app.overlays.last() else {
            panic!()
        };
        assert_eq!(picker.items().len(), 2);
        app.on_event(ServerEvent::Repos {
            project: ProjectId::new(),
            accounts: vec![],
            repos: vec![],
        });
        let Some(Overlay::Repos { picker, .. }) = app.overlays.last() else {
            panic!()
        };
        assert_eq!(
            picker.items().len(),
            2,
            "the other project's list is not shown"
        );
    }

    #[test]
    fn space_and_a_on_an_empty_repos_list_do_nothing() {
        let (mut app, _) = app();
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('m')));
        for c in [' ', 'a'] {
            let actions = app.on_key(k(K::Char(c)));
            assert!(sent(&actions).is_empty(), "{actions:?}");
            assert!(matches!(app.overlays.last(), Some(Overlay::Repos { .. })));
        }
        assert_eq!(app.overlays.len(), 1);
    }

    #[test]
    fn esc_in_the_account_list_returns_to_the_repos() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('m')));
        app.on_event(repos_event(s[0].project));
        app.on_key(k(K::Char('a')));
        assert!(matches!(
            app.overlays.last(),
            Some(Overlay::RepoAccount { .. })
        ));
        let actions = app.on_key(k(K::Esc));
        assert!(sent(&actions).is_empty());
        assert!(matches!(app.overlays.last(), Some(Overlay::Repos { .. })));
    }

    #[test]
    fn ctrl_q_closes_the_account_list_and_the_repos_together() {
        let (mut app, s) = app();
        app.on_key(k(K::Char('v')));
        app.on_key(k(K::Char('m')));
        app.on_event(repos_event(s[0].project));
        app.on_key(k(K::Char('a')));
        assert_eq!(app.overlays.len(), 2);
        app.on_key(ctrl('q'));
        assert!(app.overlays.is_empty());
    }
}
