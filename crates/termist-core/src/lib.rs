//! Pure domain model and wire protocol for termist.
pub mod codec;
pub mod config;
pub mod env;
pub mod ids;
pub mod model;
pub mod protocol;
pub mod screen;
pub mod status;

pub use ids::{ProjectId, SessionId};
pub use model::{
    EFFORT_LEVELS, Harness, HarnessInfo, LaunchOptions, ModelInfo, ProjectInfo, SessionInfo,
    SessionKind, StateSnapshot, TermColors,
};
pub use protocol::{ClientRequest, PROTOCOL_VERSION, ServerEvent};
pub use screen::{
    Cell, Color, Cursor, Modes, ScreenUpdate, Scroll, ScrollPos, Snapshot, cell_flags,
};
pub use status::{AgentStatus, Signal, attention_order, next_in_attention, now_ms};
