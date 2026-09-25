//! Pure domain model and wire protocol for termist.
pub mod ids;
pub mod model;
pub mod status;

pub use ids::{ProjectId, SessionId};
pub use model::{Harness, ProjectInfo, SessionInfo, SessionKind, StateSnapshot};
pub use status::{AgentStatus, Signal, attention_order, next_in_attention, now_ms};
