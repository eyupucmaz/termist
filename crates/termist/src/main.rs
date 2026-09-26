use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use termist_core::{ClientRequest, ServerEvent};
use termist_daemon::hook_client::run_hook;
use termist_daemon::launch::DaemonConfig;
use termist_platform::{Client, Paths};

/// terminal istanbul — mission control for your coding agents
#[derive(Parser)]
#[command(name = "termist", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background daemon (termist starts it for you)
    Daemon,
    /// Stop the daemon and every session it owns
    Kill,
    /// Forward an agent hook event to the daemon (called by agent CLIs)
    #[command(hide = true)]
    Hook {
        #[arg(long)]
        harness: String,
        event: String,
    },
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // An agent CLI treats exit 2 from a hook as "block": a malformed
        // `termist hook …` line must exit 0 silently, like every other hook failure.
        Err(_) if std::env::args_os().nth(1).is_some_and(|a| a == "hook") => {
            return ExitCode::SUCCESS;
        }
        Err(err) => err.exit(),
    };
    // A hook must never fail the agent, even if paths can't be resolved (e.g. etcetera
    // can't find a base dir and TERMIST_HOME is unset): check this before any Paths::from_env
    // error can return FAILURE.
    if let Some(Cmd::Hook { harness, event }) = &cli.cmd {
        if let Some(paths) = hook_paths() {
            hook(&paths, harness, event);
        }
        return ExitCode::SUCCESS; // a hook never fails the agent
    }
    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("termist: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async move {
        match cli.cmd {
            None => termist_tui::run::run(paths).await,
            Some(Cmd::Daemon) => {
                termist_daemon::logging::init();
                termist_daemon::server::run(paths, DaemonConfig::from_env()).await
            }
            Some(Cmd::Kill) => kill(&paths).await,
            Some(Cmd::Hook { .. }) => unreachable!("handled above"),
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("termist: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Where a hook sends its event: the daemon that spawned the agent exports its
/// runtime dir as `TERMIST_RUNTIME_DIR` (PRD §11.4), which picks the socket path and
/// the pipe name; without it, the usual `Paths::from_env()`.
fn hook_paths() -> Option<Paths> {
    let Some(dir) = std::env::var_os("TERMIST_RUNTIME_DIR").filter(|d| !d.is_empty()) else {
        return Paths::from_env().ok();
    };
    let runtime_dir = PathBuf::from(dir);
    // Only runtime_dir matters to a hook; the other dirs are never touched.
    let mut paths = Paths::from_env().unwrap_or_else(|_| Paths::under(runtime_dir.clone()));
    paths.runtime_dir = runtime_dir;
    Some(paths)
}

/// Reads the hook payload (at most 1 MiB, at most 500 ms) and forwards it within
/// 1500 ms: the whole hook stays inside its 2 s budget.
fn hook(paths: &Paths, harness: &str, event: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let payload = rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap_or_default();
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(run_hook(
        paths,
        harness,
        event,
        payload,
        std::env::var("TERMIST_SESSION_ID").ok(),
        Duration::from_millis(1500),
    ));
}

async fn kill(paths: &Paths) -> anyhow::Result<()> {
    kill_with(paths, Duration::from_secs(5)).await
}

/// How to stop a daemon that `termist kill` cannot talk to.
#[cfg(unix)]
const STOP_BY_HAND: &str = "stop it with `pkill -f 'termist daemon'`";
#[cfg(windows)]
const STOP_BY_HAND: &str = "end its termist.exe process in Task Manager";

async fn kill_with(paths: &Paths, answer_within: Duration) -> anyhow::Result<()> {
    let Ok(stream) = termist_platform::ipc::connect(paths).await else {
        println!("termist: no daemon running");
        return Ok(());
    };
    let refused = match tokio::time::timeout(answer_within, Client::handshake(stream)).await {
        Err(_) => anyhow::bail!(
            "the daemon did not answer within {} s",
            answer_within.as_secs_f32()
        ),
        Ok(Ok(mut client)) => {
            client.send(&ClientRequest::Shutdown).await?;
            // an Ack, or the daemon going away before it could send one
            shutdown_answer(client, answer_within).await?;
            println!("termist: daemon stopped");
            return Ok(());
        }
        Ok(Err(e)) => e,
    };
    // Something is listening but refused the handshake: a daemon of another version.
    // A bare Shutdown (no Hello) stops daemons that accept one; older ones just close
    // the connection, which is harmless.
    if let Ok(stream) = termist_platform::ipc::connect(paths).await {
        let mut client = Client::without_handshake(stream);
        if client.send(&ClientRequest::Shutdown).await.is_ok()
            && let Ok(true) = shutdown_answer(client, answer_within).await
        {
            println!("termist: daemon stopped");
            return Ok(());
        }
    }
    anyhow::bail!(
        "a termist daemon of a different version is running ({refused:#}); {STOP_BY_HAND}"
    )
}

/// Waits for the answer to a Shutdown: true for an Ack, false when the daemon closed
/// the connection without one.
async fn shutdown_answer(mut client: Client, answer_within: Duration) -> anyhow::Result<bool> {
    let answer = tokio::time::timeout(answer_within, async {
        while let Some(ev) = client.recv().await? {
            if ev == ServerEvent::Ack {
                return anyhow::Ok(true);
            }
        }
        anyhow::Ok(false)
    })
    .await;
    match answer {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "the daemon did not answer within {} s",
            answer_within.as_secs_f32()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::tokio::prelude::*;
    use termist_platform::framed::{FramedReader, write_frame};

    fn test_paths(tmp: &tempfile::TempDir) -> Paths {
        let paths = Paths::under(tmp.path().to_path_buf());
        paths.ensure().unwrap();
        paths
    }

    /// Bounds a `kill_with` call so a hang fails the test instead of stalling it.
    async fn bounded(paths: &Paths) -> anyhow::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(3),
            kill_with(paths, Duration::from_millis(300)),
        )
        .await
        .expect("kill must not hang")
    }

    /// A stand-in for a daemon of another protocol version: it refuses every Hello
    /// with an error, and acknowledges a Shutdown sent as the first frame only when
    /// `obeys_shutdown` (older daemons just close the connection); each obeyed
    /// Shutdown is reported on the returned channel.
    fn other_version_daemon(
        paths: &Paths,
        obeys_shutdown: bool,
    ) -> tokio::sync::mpsc::UnboundedReceiver<()> {
        let listener = termist_platform::ipc::listen(paths).unwrap();
        let (stopped, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let conn = listener.accept().await.unwrap();
                let (r, mut w) = conn.split();
                let mut reader = FramedReader::new(r);
                match reader.read::<ClientRequest>().await {
                    Ok(Some(ClientRequest::Hello { .. })) => {
                        let message = "protocol 2 is not supported (daemon pid 7 speaks 3)";
                        let _ = write_frame(
                            &mut w,
                            &ServerEvent::Error {
                                message: message.into(),
                            },
                        )
                        .await;
                    }
                    Ok(Some(ClientRequest::Shutdown)) if obeys_shutdown => {
                        let _ = write_frame(&mut w, &ServerEvent::Ack).await;
                        let _ = stopped.send(());
                    }
                    _ => {}
                }
            }
        });
        rx
    }

    #[tokio::test]
    async fn kill_without_a_daemon_says_so_and_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(bounded(&test_paths(&tmp)).await.is_ok());
    }

    #[tokio::test]
    async fn kill_gives_up_when_the_daemon_never_answers_the_hello() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);
        let listener = termist_platform::ipc::listen(&paths).unwrap();
        tokio::spawn(async move {
            let _conn = listener.accept().await.unwrap();
            std::future::pending::<()>().await; // never answer, never close
        });
        let err = bounded(&paths).await.unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err}");
    }

    #[tokio::test]
    async fn kill_reports_a_daemon_of_another_version_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);
        other_version_daemon(&paths, false);
        let err = format!("{:#}", bounded(&paths).await.unwrap_err());
        assert!(err.contains("different version"), "{err}");
        assert!(
            err.contains("daemon pid 7"),
            "the daemon's own words: {err}"
        );
        #[cfg(unix)]
        assert!(err.contains("pkill -f 'termist daemon'"), "{err}");
    }

    #[tokio::test]
    async fn kill_stops_a_daemon_of_another_version_that_takes_a_bare_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(&tmp);
        let mut stopped = other_version_daemon(&paths, true);
        assert!(bounded(&paths).await.is_ok());
        assert!(
            stopped.try_recv().is_ok(),
            "the Shutdown reached the daemon"
        );
    }

    /// `termist kill` against a daemon that accepts but never answers must give up.
    #[tokio::test]
    async fn kill_gives_up_when_the_daemon_never_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        paths.ensure().unwrap();
        let listener = termist_platform::ipc::listen(&paths).unwrap();
        tokio::spawn(async move {
            use interprocess::local_socket::tokio::prelude::*;
            let conn = listener.accept().await.unwrap();
            let (r, mut w) = conn.split();
            let mut reader = termist_platform::framed::FramedReader::new(r);
            let _hello: Option<ClientRequest> = reader.read().await.unwrap();
            termist_platform::framed::write_frame(
                &mut w,
                &ServerEvent::Hello {
                    version: termist_core::PROTOCOL_VERSION,
                    pid: 1,
                },
            )
            .await
            .unwrap();
            let _shutdown: Option<ClientRequest> = reader.read().await.unwrap();
            std::future::pending::<()>().await; // never Ack, never close
        });
        let started = std::time::Instant::now();
        let err = kill_with(&paths, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
