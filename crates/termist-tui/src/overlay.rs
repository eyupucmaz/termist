//! What is open on top of the grid or the focused pane. The top of `App::overlays`
//! gets every key; Esc closes it and Ctrl+Q closes them all, so a picker opened from
//! the quick prompt returns to it with the text still there.
use crate::list_picker::ListPicker;
use crate::text_input::TextInput;
use termist_core::{Harness, HarnessInfo, LaunchOptions, ProjectId, ProjectInfo, SessionId};

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
            _ => None,
        }
    }
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
pub struct QuickPrompt {
    pub input: TextInput,
    pub project: ProjectId,
    pub launch: LaunchOptions,
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
    Named(String),
    /// Opens a text box for any model name.
    Type,
}

impl ModelChoice {
    pub fn label(&self) -> String {
        match self {
            ModelChoice::Default => "CLI default".into(),
            ModelChoice::Named(m) => m.clone(),
            ModelChoice::Type => "type a model…".into(),
        }
    }

    pub fn model(&self) -> Option<String> {
        match self {
            ModelChoice::Named(m) => Some(m.clone()),
            ModelChoice::Default | ModelChoice::Type => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPicker {
    pub harness: Harness,
    pub models: ListPicker<ModelChoice>,
    /// 0 is the CLI's default, then `harness.efforts()` in order.
    pub effort: usize,
}

impl ModelPicker {
    pub fn new(launch: &LaunchOptions, recent: Vec<String>) -> ModelPicker {
        let efforts = launch.harness.efforts();
        let effort = launch
            .effort
            .as_deref()
            .and_then(|e| efforts.iter().position(|x| *x == e))
            .map_or(0, |i| i + 1);
        let mut picker = ModelPicker {
            harness: launch.harness,
            models: ListPicker::new(vec![], ModelChoice::label, false),
            effort,
        };
        picker.set_recent(recent, launch.model.as_deref());
        if let Some(current) = &launch.model {
            let at = picker
                .models
                .items()
                .iter()
                .position(|m| *m == ModelChoice::Named(current.clone()));
            picker.models.select_index(at.unwrap_or(0));
        }
        picker
    }

    /// The list is: CLI default, the current model (unless it is a recent one), the
    /// recent models, then "type a model…". The highlighted entry stays highlighted.
    pub fn set_recent(&mut self, recent: Vec<String>, current: Option<&str>) {
        let keep = self.models.selected().cloned();
        let mut items = vec![ModelChoice::Default];
        if let Some(c) = current.filter(|c| !recent.iter().any(|r| r == c)) {
            items.push(ModelChoice::Named(c.to_string()));
        }
        items.extend(recent.into_iter().map(ModelChoice::Named));
        items.push(ModelChoice::Type);
        self.models.set_items(items, ModelChoice::label);
        if let Some(i) = keep.and_then(|k| self.models.items().iter().position(|m| *m == k)) {
            self.models.select_index(i);
        }
    }

    pub fn step_effort(&mut self, delta: isize) {
        let last = self.harness.efforts().len() as isize;
        self.effort = (self.effort as isize + delta).clamp(0, last) as usize;
    }

    pub fn effort(&self) -> Option<String> {
        self.effort
            .checked_sub(1)
            .and_then(|i| self.harness.efforts().get(i))
            .map(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_model_list_is_default_then_recent_then_type_your_own() {
        let m = ModelPicker::new(
            &launch(Harness::Claude, Some("opus"), Some("high")),
            vec!["sonnet".into(), "opus".into()],
        );
        assert_eq!(
            labels(&m),
            ["CLI default", "sonnet", "opus", "type a model…"]
        );
        assert_eq!(
            m.models.selected(),
            Some(&ModelChoice::Named("opus".into()))
        );
        assert_eq!(m.effort().as_deref(), Some("high"));
        let typed = ModelPicker::new(&launch(Harness::Codex, Some("my-model"), None), vec![]);
        assert_eq!(labels(&typed), ["CLI default", "my-model", "type a model…"]);
        assert_eq!(typed.effort(), None);
    }

    #[test]
    fn recent_models_arriving_later_keep_the_highlight() {
        let mut m = ModelPicker::new(&launch(Harness::Claude, None, None), vec![]);
        m.models.select_index(1); // "type a model…"
        m.set_recent(vec!["opus".into()], None);
        assert_eq!(m.models.selected(), Some(&ModelChoice::Type));
    }

    #[test]
    fn effort_steps_stop_at_both_ends_and_opencode_has_none() {
        let mut m = ModelPicker::new(&launch(Harness::Codex, None, None), vec![]);
        m.step_effort(-1);
        assert_eq!(m.effort(), None);
        for _ in 0..9 {
            m.step_effort(1);
        }
        assert_eq!(m.effort().as_deref(), Some("high"));
        let mut o = ModelPicker::new(&launch(Harness::OpenCode, None, None), vec![]);
        o.step_effort(1);
        assert_eq!(o.effort(), None);
    }

    #[test]
    fn another_harness_drops_the_model_and_an_effort_it_does_not_know() {
        let mut q = QuickPrompt {
            input: TextInput::new(true),
            project: ProjectId::new(),
            launch: launch(Harness::Claude, Some("opus"), Some("high")),
        };
        q.set_harness(Harness::Codex);
        assert_eq!(q.launch, launch(Harness::Codex, None, Some("high")));
        q.launch.effort = Some("medium".into());
        q.set_harness(Harness::OpenCode);
        assert_eq!(q.launch, launch(Harness::OpenCode, None, None));
    }
}
