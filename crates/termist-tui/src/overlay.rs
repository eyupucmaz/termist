//! What is open on top of the grid or the focused pane. The top of `App::overlays`
//! gets every key; Esc closes it and Ctrl+Q closes them all, so a picker opened from
//! the quick prompt returns to it with the text still there.
use crate::list_picker::ListPicker;
use termist_core::HarnessInfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// `n`, or Tab in the quick prompt: choose an agent CLI.
    Harness(ListPicker<HarnessInfo>),
}

impl Overlay {
    pub fn harness_picker_mut(&mut self) -> Option<&mut ListPicker<HarnessInfo>> {
        match self {
            Overlay::Harness(picker) => Some(picker),
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
