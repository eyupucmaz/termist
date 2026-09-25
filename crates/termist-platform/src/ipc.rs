use crate::paths::Paths;
use interprocess::local_socket::tokio::prelude::*;
pub use interprocess::local_socket::tokio::{Listener, RecvHalf, SendHalf, Stream};
use interprocess::local_socket::{ListenerOptions, Name};
use std::io;

fn name(paths: &Paths) -> io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        paths
            .socket_path()
            .to_string_lossy()
            .into_owned()
            .to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        paths.pipe_name().to_ns_name::<GenericNamespaced>()
    }
}

/// Binds the daemon socket. On unix a leftover socket file is removed first; callers
/// must check that no live daemon owns it (see `termist_daemon::server::run`).
pub fn listen(paths: &Paths) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        let path = paths.socket_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    ListenerOptions::new().name(name(paths)?).create_tokio()
}

pub async fn connect(paths: &Paths) -> io::Result<Stream> {
    Stream::connect(name(paths)?).await
}
