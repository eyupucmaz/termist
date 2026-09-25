//! OS boundary: paths, local sockets, framing, client.
pub mod client;
pub mod framed;
pub mod ipc;
pub mod paths;

pub use client::Client;
pub use paths::Paths;
