//! What is open on top of the grid or the focused pane. The top of `App::overlays`
//! gets every key; Esc closes it and Ctrl+Q closes them all, so a picker opened from
//! the quick prompt returns to it with the text still there.
use crate::browse::{DirEntry, Listing};
use crate::keys::{self, Action as KeyAction, Context, KeySpec};
use crate::list_picker::ListPicker;
use crate::text_input::TextInput;
use std::path::PathBuf;
use termist_core::github::{RepoId, RepoInfo};
use termist_core::{
    Harness, HarnessInfo, LaunchOptions, ModelInfo, ProjectId, ProjectInfo, SessionId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// `n`, or Tab in the quick prompt: choose an agent CLI.
    Harness(ListPicker<HarnessInfo>),
    /// `p`: a new task, with its launch line.
    QuickPrompt(QuickPrompt),
    /// Ctrl+O in the quick prompt: model and effort.
    Model(ModelPicker),
    /// "type a model…" in the model picker.
    ModelName(TextInput),
    /// Ctrl+P in the quick prompt: the project to start in.
    Project(ListPicker<ProjectInfo>),
    /// `a` in a pull request: who its review threads go to, as `text`.
    Hand {
        pr: termist_core::github::PrRef,
        text: String,
        picker: ListPicker<HandTo>,
    },
    /// `Ctrl+T` in the quick prompt: where the task starts.
    Target(ListPicker<TargetChoice>),
    /// `Space`: the next instruction for a card's agent, sent without entering the card.
    FollowUp {
        session: SessionId,
        input: TextInput,
    },
    /// `r`: a new name for a card.
    Rename {
        session: SessionId,
        input: TextInput,
    },
    /// `/` and `C-a /`: every session in the open projects, by attention.
    Palette(ListPicker<SessionId>),
    /// `o`: the known projects, then a folder browser.
    OpenProject(OpenProject),
    /// `?` and `C-a ?`: every key, scrolled down `scroll` lines.
    Help { scroll: usize },
    /// `s`: theme, colours, prefix and the way to the keys.
    Settings(SettingsView),
    /// "keys" in the settings: every grid and focus action with its keys.
    Keys(SettingsView),
    /// Waiting for the key to bind.
    KeyCapture(Capture),
    /// `m` in the pull requests: the project's repos, shown or hidden, and their accounts.
    Repos {
        project: ProjectId,
        picker: ListPicker<RepoInfo>,
    },
    /// `a` in the repos: the account to read a repo with; `None` is the one with the
    /// most access.
    RepoAccount {
        repo: RepoId,
        picker: ListPicker<Option<String>>,
    },
    /// `c`, `r`, `e` and `A` in a pull request: a comment, a reply, an edit or a review.
    Compose(crate::prs::compose::Compose),
}

pub fn repo_label(r: &RepoInfo) -> String {
    r.name.clone()
}

/// A list of settings: the highlighted row, and a word about the last change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub row: usize,
    pub note: Option<String>,
}

/// The rows of the settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingRow {
    Theme,
    Colors,
    Prefix,
    Pane,
    Keys,
    DoneSound,
    WaitingSound,
    Desktop,
    Toasts,
    Splash,
    Idle,
    Animations,
    Mouse,
    StatusCpu,
    StatusRam,
    StatusBattery,
    StatusClock,
    /// Pull requests through gh.
    PullRequests,
    /// A pull request's diff: unified or split.
    DiffLayout,
}

pub const SETTING_ROWS: [SettingRow; 19] = [
    SettingRow::Theme,
    SettingRow::Colors,
    SettingRow::Prefix,
    SettingRow::Pane,
    SettingRow::Keys,
    SettingRow::DoneSound,
    SettingRow::WaitingSound,
    SettingRow::Desktop,
    SettingRow::Toasts,
    SettingRow::Splash,
    SettingRow::Idle,
    SettingRow::Animations,
    SettingRow::Mouse,
    SettingRow::StatusCpu,
    SettingRow::StatusRam,
    SettingRow::StatusBattery,
    SettingRow::StatusClock,
    SettingRow::PullRequests,
    SettingRow::DiffLayout,
];

/// What a captured key will be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    Prefix,
    Key(Context, KeyAction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub target: CaptureTarget,
    /// A key pressed that another action has: Enter gives it to this one and the other
    /// action this one's old key.
    pub conflict: Option<(KeySpec, KeyAction)>,
    pub note: Option<String>,
}

/// The rows of the keys screen: the grid's actions, then focus mode's.
pub fn key_rows() -> Vec<(Context, KeyAction)> {
    keys::actions(Context::Grid)
        .iter()
        .map(|a| (Context::Grid, *a))
        .chain(
            keys::actions(Context::Focus)
                .iter()
                .map(|a| (Context::Focus, *a)),
        )
        .collect()
}

impl Overlay {
    pub fn harness_picker_mut(&mut self) -> Option<&mut ListPicker<HarnessInfo>> {
        match self {
            Overlay::Harness(picker) => Some(picker),
            _ => None,
        }
    }

    /// The text box a paste goes into, if this overlay has one.
    pub fn text_input_mut(&mut self) -> Option<&mut TextInput> {
        match self {
            Overlay::QuickPrompt(q) => Some(&mut q.input),
            Overlay::ModelName(input)
            | Overlay::FollowUp { input, .. }
            | Overlay::Rename { input, .. } => Some(input),
            Overlay::Compose(c) => Some(&mut c.input),
            _ => None,
        }
    }
}

/// Where review threads go: a card on the pull request's branch, or a new agent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HandTo {
    Card(termist_core::SessionId),
    New,
}

/// The harness picker, opened on the first CLI that is installed.
pub fn harness_picker(harnesses: &[HarnessInfo]) -> ListPicker<HarnessInfo> {
    let mut picker = ListPicker::new(harnesses.to_vec(), harness_label, false);
    if let Some(first) = harnesses.iter().position(|h| h.available) {
        picker.select_index(first);
    }
    picker
}

pub fn harness_label(h: &HarnessInfo) -> String {
    h.harness.id().to_string()
}

/// A project picker over `projects`, filtered by typing, on `current`.
pub fn project_picker(projects: Vec<ProjectInfo>, current: ProjectId) -> ListPicker<ProjectInfo> {
    let mut picker = ListPicker::new(projects, |p| p.name.clone(), true);
    if let Some(i) = picker.items().iter().position(|p| p.id == current) {
        picker.select_index(i);
    }
    picker
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseEntry {
    Project(ProjectInfo),
    Dir(DirEntry),
}

impl BrowseEntry {
    pub fn label(&self) -> String {
        match self {
            BrowseEntry::Project(p) => p.name.clone(),
            BrowseEntry::Dir(d) => d.name.clone(),
        }
    }
}

/// Known projects on top (closed ones first), the folders of `dir` below them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenProject {
    pub dir: PathBuf,
    pub list: ListPicker<BrowseEntry>,
    projects: Vec<ProjectInfo>,
    /// The folders of `dir` are on their way.
    pub loading: bool,
    pub error: Option<String>,
    /// More folders than are shown.
    pub truncated: bool,
}

impl OpenProject {
    pub fn new(projects: &[ProjectInfo], dir: PathBuf) -> OpenProject {
        let mut projects = projects.to_vec();
        projects.sort_by_key(|p| p.open);
        let mut open = OpenProject {
            dir: PathBuf::new(),
            list: ListPicker::new(vec![], BrowseEntry::label, true),
            projects,
            loading: false,
            error: None,
            truncated: false,
        };
        open.enter(dir);
        open
    }

    /// Moves to `dir`: its folders are unknown until `listed` gets them.
    pub fn enter(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.loading = true;
        self.error = None;
        self.truncated = false;
        self.list = ListPicker::new(self.entries(vec![]), BrowseEntry::label, true);
    }

    /// The folders of `self.dir`, or why they could not be read.
    pub fn listed(&mut self, listing: Result<Listing, String>) {
        self.loading = false;
        let dirs = match listing {
            Ok(listing) => {
                self.truncated = listing.truncated;
                listing.entries
            }
            Err(e) => {
                self.error = Some(e);
                vec![]
            }
        };
        let entries = self.entries(dirs);
        self.list.set_items(entries, BrowseEntry::label);
    }

    fn entries(&self, dirs: Vec<DirEntry>) -> Vec<BrowseEntry> {
        self.projects
            .iter()
            .cloned()
            .map(BrowseEntry::Project)
            .chain(dirs.into_iter().map(BrowseEntry::Dir))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickPrompt {
    pub input: TextInput,
    pub project: ProjectId,
    pub launch: LaunchOptions,
    /// The worktree the agent starts in and its branch (from `w`); `None` is the
    /// project's folder.
    pub worktree: Option<(std::path::PathBuf, String)>,
    /// `Ctrl+N`: a new worktree, on a branch named from the prompt, in this repo.
    pub new_worktree: Option<NewWorktree>,
}

/// A worktree to make for the task: in which repo (`repo@` before the branch where the
/// project holds several), and the seed of a name when the prompt gives none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWorktree {
    pub repo: termist_core::github::RepoId,
    pub repo_name: Option<String>,
    pub seed: u64,
}

/// Where a task starts, as `Ctrl+T` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TargetChoice {
    /// The project's own folder.
    Folder,
    /// A worktree: its folder and its branch (or folder) name.
    Worktree(std::path::PathBuf, String),
    /// A new worktree of a repo (named when the project holds several).
    New(termist_core::github::RepoId, Option<String>),
}

impl QuickPrompt {
    /// Another CLI keeps the effort if it knows that level; a model name belongs to
    /// one CLI, so it goes.
    pub fn set_harness(&mut self, harness: Harness) {
        if harness == self.launch.harness {
            return;
        }
        self.launch.harness = harness;
        self.launch.model = None;
        self.launch.effort = self
            .launch
            .effort
            .take()
            .filter(|e| harness.efforts().contains(&e.as_str()));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// No model flag: the CLI decides.
    Default,
    Named(ModelInfo),
    /// Opens a text box for any model name.
    Type,
}

impl ModelChoice {
    /// A model known only by its id: a recent one, or one typed.
    pub fn named(id: &str) -> ModelChoice {
        ModelChoice::Named(ModelInfo {
            id: id.into(),
            label: id.into(),
            efforts: vec![],
        })
    }

    pub fn label(&self) -> String {
        match self {
            ModelChoice::Default => "CLI default".into(),
            ModelChoice::Named(m) => m.label.clone(),
            ModelChoice::Type => "type a model…".into(),
        }
    }

    pub fn model(&self) -> Option<String> {
        match self {
            ModelChoice::Named(m) => Some(m.id.clone()),
            ModelChoice::Default | ModelChoice::Type => None,
        }
    }

    /// "CLI default" and "type a model…" show whatever is typed.
    pub fn is_pinned(&self) -> bool {
        !matches!(self, ModelChoice::Named(_))
    }

    /// The same entry across lists: a model by its id, whatever its label.
    fn key(&self) -> Option<String> {
        match self {
            ModelChoice::Default => Some(String::new()),
            ModelChoice::Named(m) => Some(m.id.clone()),
            ModelChoice::Type => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPicker {
    pub harness: Harness,
    pub models: ListPicker<ModelChoice>,
    /// The model in use when the picker opened; listed even when no list has it.
    pub current: Option<String>,
    /// The effort asked for; `None` is the CLI's default. A model that does not take it
    /// gets the nearest level below.
    wanted_effort: Option<String>,
}

impl ModelPicker {
    pub fn new(launch: &LaunchOptions, recent: Vec<String>, catalog: &[ModelInfo]) -> ModelPicker {
        let mut picker = ModelPicker {
            harness: launch.harness,
            models: ListPicker::new(vec![], ModelChoice::label, true)
                .pinned(ModelChoice::is_pinned),
            current: launch.model.clone(),
            wanted_effort: launch.effort.clone(),
        };
        picker.set_lists(recent, catalog, launch.model.as_deref());
        if let Some(current) = &launch.model {
            let at = picker
                .models
                .items()
                .iter()
                .position(|m| m.model().as_deref() == Some(current.as_str()));
            picker.models.select_index(at.unwrap_or(0));
        }
        picker
    }

    /// CLI default, the current model (unless listed below), the recent models, the
    /// CLI's own list, then "type a model…"; a model shows once, with the CLI's label.
    /// The highlighted entry stays highlighted.
    pub fn set_lists(&mut self, recent: Vec<String>, catalog: &[ModelInfo], current: Option<&str>) {
        let keep = self.models.selected().map(ModelChoice::key);
        let info = |id: &str| {
            catalog
                .iter()
                .find(|m| m.id == id)
                .cloned()
                .map_or_else(|| ModelChoice::named(id), ModelChoice::Named)
        };
        let mut items = vec![ModelChoice::Default];
        let listed =
            |id: &str| recent.iter().any(|r| r == id) || catalog.iter().any(|m| m.id == id);
        if let Some(c) = current.filter(|c| !listed(c)) {
            items.push(ModelChoice::named(c));
        }
        let mut seen: Vec<String> = Vec::new();
        for id in recent
            .iter()
            .map(String::as_str)
            .chain(catalog.iter().map(|m| m.id.as_str()))
        {
            if !seen.iter().any(|s| s == id) {
                seen.push(id.to_string());
                items.push(info(id));
            }
        }
        items.push(ModelChoice::Type);
        self.models.set_items(items, ModelChoice::label);
        if let Some(i) = keep.and_then(|k| self.models.items().iter().position(|m| m.key() == k)) {
            self.models.select_index(i);
        }
    }

    /// The effort levels of the highlighted model, or the harness's own.
    pub fn efforts(&self) -> Vec<String> {
        match self.models.selected() {
            Some(ModelChoice::Named(m)) if !m.efforts.is_empty() => m.efforts.clone(),
            _ => self
                .harness
                .efforts()
                .iter()
                .map(|e| e.to_string())
                .collect(),
        }
    }

    pub fn effort(&self) -> Option<String> {
        let wanted = self.wanted_effort.as_deref()?;
        let levels = self.efforts();
        if levels.iter().any(|l| l == wanted) {
            return Some(wanted.to_string());
        }
        let rank = |l: &str| termist_core::EFFORT_LEVELS.iter().position(|x| *x == l);
        let top = rank(wanted)?;
        levels
            .into_iter()
            .filter(|l| rank(l).is_some_and(|r| r < top))
            .max_by_key(|l| rank(l))
    }

    /// 0 for the CLI's default, then 1… for `efforts()` in order.
    pub fn effort_index(&self) -> usize {
        let levels = self.efforts();
        self.effort()
            .and_then(|e| levels.iter().position(|l| *l == e))
            .map_or(0, |i| i + 1)
    }

    pub fn step_effort(&mut self, delta: isize) {
        let levels = self.efforts();
        let at = (self.effort_index() as isize + delta).clamp(0, levels.len() as isize) as usize;
        self.wanted_effort = at.checked_sub(1).map(|i| levels[i].clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use termist_core::ModelInfo;

    fn launch(harness: Harness, model: Option<&str>, effort: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            harness,
            model: model.map(Into::into),
            effort: effort.map(Into::into),
        }
    }

    fn labels(m: &ModelPicker) -> Vec<String> {
        m.models.items().iter().map(ModelChoice::label).collect()
    }

    fn info(id: &str, label: &str, efforts: &[&str]) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            label: label.into(),
            efforts: efforts.iter().map(|e| e.to_string()).collect(),
        }
    }

    fn codex_catalog() -> Vec<ModelInfo> {
        vec![
            info(
                "gpt-6-astra",
                "GPT-6-Astra",
                &["low", "medium", "high", "xhigh", "max", "ultra"],
            ),
            info("gpt-5.5", "GPT-5.5", &["low", "medium", "high", "xhigh"]),
        ]
    }

    #[test]
    fn the_list_is_default_current_recent_catalog_then_type_your_own() {
        let m = ModelPicker::new(
            &launch(Harness::Codex, Some("my-model"), Some("high")),
            vec!["gpt-5.5".into(), "old-model".into()],
            &codex_catalog(),
        );
        assert_eq!(
            labels(&m),
            [
                "CLI default",
                "my-model",
                "GPT-5.5",
                "old-model",
                "GPT-6-Astra",
                "type a model…"
            ]
        );
        assert_eq!(m.models.selected(), Some(&ModelChoice::named("my-model")));
        assert_eq!(m.effort().as_deref(), Some("high"));
    }

    #[test]
    fn the_catalog_arriving_later_keeps_the_highlight() {
        let mut m = ModelPicker::new(
            &launch(Harness::Codex, None, None),
            vec!["gpt-5.5".into()],
            &[],
        );
        m.models.select_index(1);
        m.set_lists(vec!["gpt-5.5".into()], &codex_catalog(), None);
        assert_eq!(
            m.models.selected().map(ModelChoice::label).as_deref(),
            Some("GPT-5.5"),
            "the same model, now with its name"
        );
    }

    #[test]
    fn rows_arriving_above_the_highlight_do_not_move_it() {
        let mut m = ModelPicker::new(
            &launch(Harness::Codex, None, None),
            vec!["gpt-5.5".into()],
            &[],
        );
        m.models.select_index(2);
        assert_eq!(m.models.selected(), Some(&ModelChoice::Type));
        m.set_lists(vec!["gpt-5.5".into()], &codex_catalog(), None);
        assert_eq!(
            m.models.selected(),
            Some(&ModelChoice::Type),
            "the catalog added a row above it"
        );
    }

    #[test]
    fn filtering_keeps_default_and_type_your_own() {
        let mut m = ModelPicker::new(
            &launch(Harness::Codex, None, None),
            vec![],
            &codex_catalog(),
        );
        for c in "astra".chars() {
            m.models.key(KeyEvent::from(KeyCode::Char(c)));
        }
        let visible: Vec<String> = m.models.visible().map(|(_, c, _)| c.label()).collect();
        assert_eq!(visible, ["CLI default", "GPT-6-Astra", "type a model…"]);
        for c in "zzz".chars() {
            m.models.key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(m.models.visible_len(), 2);
    }

    #[test]
    fn efforts_follow_the_model_and_step_down_when_it_has_less() {
        let mut m = ModelPicker::new(
            &launch(Harness::Codex, None, None),
            vec![],
            &codex_catalog(),
        );
        m.models.key(KeyEvent::from(KeyCode::Down));
        assert_eq!(m.efforts().len(), 6, "gpt-6-astra");
        for _ in 0..9 {
            m.step_effort(1);
        }
        assert_eq!(m.effort().as_deref(), Some("ultra"));
        m.models.key(KeyEvent::from(KeyCode::Down));
        assert_eq!(
            m.effort().as_deref(),
            Some("xhigh"),
            "gpt-5.5 tops out at xhigh"
        );
        assert_eq!(m.effort_index(), 4);
        m.step_effort(-9);
        assert_eq!(m.effort(), None);
    }

    #[test]
    fn opencode_has_no_effort() {
        let mut o = ModelPicker::new(&launch(Harness::OpenCode, None, None), vec![], &[]);
        o.step_effort(1);
        assert_eq!(o.effort(), None);
        assert!(o.efforts().is_empty());
    }

    #[test]
    fn another_harness_drops_the_model_and_an_effort_it_does_not_know() {
        let mut q = QuickPrompt {
            input: TextInput::new(true),
            project: ProjectId::new(),
            launch: launch(Harness::Claude, Some("opus"), Some("high")),
            worktree: None,
            new_worktree: None,
        };
        q.set_harness(Harness::Codex);
        assert_eq!(q.launch, launch(Harness::Codex, None, Some("high")));
        q.launch.effort = Some("medium".into());
        q.set_harness(Harness::OpenCode);
        assert_eq!(q.launch, launch(Harness::OpenCode, None, None));
    }
}
