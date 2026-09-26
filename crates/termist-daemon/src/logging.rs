//! The daemon logs through `tracing` to stderr, which `termist` redirects to
//! `<data_dir>/daemon.log` when it autostarts the daemon. `TERMIST_LOG` sets the filter.
use tracing_subscriber::EnvFilter;

pub fn init() {
    let filter = EnvFilter::try_from_env("TERMIST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init();
}
