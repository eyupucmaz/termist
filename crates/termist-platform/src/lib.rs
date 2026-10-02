//! OS boundary: paths, local sockets, framing, client.
pub mod client;
pub mod clipboard;
pub mod clock;
pub mod config_file;
pub mod framed;
pub mod host_colors;
pub mod ipc;
pub mod notify;
pub mod paths;
pub mod term;

pub use client::Client;
pub use paths::Paths;
