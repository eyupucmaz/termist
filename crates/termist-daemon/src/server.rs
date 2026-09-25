use crate::claude;
use crate::launch::{DaemonConfig, Launcher};
use crate::registry::{self, Msg, Registry};
use crate::session::ClientId;
use anyhow::bail;
use interprocess::local_socket::tokio::prelude::*;
use std::fs::{File, TryLockError};
use std::path::PathBuf;
use std::time::Duration;
use termist_core::{ClientRequest, PROTOCOL_VERSION, ServerEvent};
use termist_platform::framed::{FramedReader, write_frame};
use termist_platform::ipc::{self, Stream};
use termist_platform::{Client, Paths};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

pub async fn run(paths: Paths, config: DaemonConfig) -> anyhow::Result<()> {
    paths.ensure()?;

    // An exclusive lock closes the race where two daemons both probe the socket, see
    // nobody home, and then both try to bind: only one of them can hold this lock, so
    // the loser bails out before it can unlink the winner's socket out from under it.
    // The `File` is bound to `lock` (not `_`) so the lock is held for all of `run`.
    let lock_path = paths.runtime_dir.join("daemon.lock");
    let lock = File::create(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            bail!(
                "a termist daemon is already running for {}",
                paths.runtime_dir.display()
            );
        }
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }

    if Client::connect(&paths).await.is_ok() {
        bail!(
            "a termist daemon is already running for {}",
            paths.runtime_dir.display()
        );
    }
    let listener = ipc::listen(&paths)?;
    let exe = std::env::current_exe()?;
    let claude_settings = claude::write_settings(&paths, &exe)?;
    let (tx, rx) = unbounded_channel();
    let (notes_tx, notes_rx) = unbounded_channel();
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let launcher = Launcher {
        config,
        exe,
        claude_settings,
        runtime_dir: paths.runtime_dir.clone(),
        termist_home: std::env::var_os("TERMIST_HOME").map(PathBuf::from),
    };
    tokio::spawn(registry::run(
        Registry::new(launcher, notes_tx, stop_tx),
        rx,
        notes_rx,
    ));

    let mut next = 0u64;
    loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok(conn) => {
                    next += 1;
                    tokio::spawn(connection(ClientId(next), conn, tx.clone()));
                }
                // One failed accept (EMFILE, ECONNABORTED, a broken pipe instance on
                // Windows, …) must not take every session down with the daemon: log it
                // (stderr is the daemon log), back off briefly and keep serving.
                Err(e) => {
                    eprintln!("termist daemon: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            _ = &mut stop_rx => break,
        }
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(paths.socket_path());
    Ok(())
}

async fn connection(client: ClientId, conn: Stream, registry: UnboundedSender<Msg>) {
    let (r, mut w) = conn.split();
    let mut reader = FramedReader::new(r);
    match reader.read::<ClientRequest>().await {
        Ok(Some(ClientRequest::Hello { version })) if version == PROTOCOL_VERSION => {}
        Ok(Some(ClientRequest::Hello { version })) => {
            let message = format!(
                "protocol {version} is not supported (daemon speaks {PROTOCOL_VERSION}); run `termist kill` and start again"
            );
            let _ = write_frame(&mut w, &ServerEvent::Error { message }).await;
            return;
        }
        _ => return,
    }
    if write_frame(
        &mut w,
        &ServerEvent::Hello {
            version: PROTOCOL_VERSION,
            pid: std::process::id(),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let (out_tx, mut out_rx) = unbounded_channel::<ServerEvent>();
    let _ = registry.send(Msg::Connected {
        client,
        out: out_tx,
    });
    let writer = tokio::spawn(async move {
        while let Some(event) = out_rx.recv().await {
            if write_frame(&mut w, &event).await.is_err() {
                break;
            }
        }
    });
    while let Ok(Some(req)) = reader.read::<ClientRequest>().await {
        let _ = registry.send(Msg::Request { client, req });
    }
    let _ = registry.send(Msg::Disconnected(client));
    writer.abort();
}
