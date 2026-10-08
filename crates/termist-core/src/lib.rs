//! Pure domain model and wire protocol for termist.
pub mod codec;
pub mod config;
pub mod diff;
pub mod env;
pub mod github;
pub mod ids;
pub mod model;
pub mod protocol;
pub mod screen;
pub mod status;

pub use ids::{ProjectId, SessionId};
pub use model::{
    DiffMode, EFFORT_LEVELS, Harness, HarnessInfo, LaunchOptions, LocalDiffData, ModelInfo, Place,
    PrEnd, ProjectInfo, ReadState, SessionInfo, SessionKind, Stat, StateSnapshot, TermColors,
    WorktreeInfo,
};
pub use protocol::{ClientRequest, PROTOCOL_VERSION, ServerEvent};
pub use screen::{
    Cell, Color, Cursor, Modes, ScreenUpdate, Scroll, ScrollPos, Snapshot, cell_flags,
};
pub use status::{AgentStatus, Signal, attention_order, next_in_attention, now_ms};
