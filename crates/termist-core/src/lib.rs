//! Pure domain model and wire protocol for termist.
pub mod codec;
pub mod env;
pub mod ids;
pub mod model;
pub mod protocol;
pub mod screen;
pub mod status;

pub use ids::{ProjectId, SessionId};
pub use model::{Harness, HarnessInfo, ProjectInfo, SessionInfo, SessionKind, StateSnapshot};
pub use protocol::{ClientRequest, PROTOCOL_VERSION, ServerEvent};
pub use screen::{Cell, Color, Cursor, Modes, ScreenUpdate, Snapshot, cell_flags};
pub use status::{AgentStatus, Signal, attention_order, next_in_attention, now_ms};
