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

async fn kill_with(paths: &Paths, answer_within: Duration) -> anyhow::Result<()> {
    let Ok(mut client) = Client::connect(paths).await else {
        println!("termist: no daemon running");
        return Ok(());
    };
    client.send(&ClientRequest::Shutdown).await?;
    let acked = tokio::time::timeout(answer_within, async {
        while let Some(ev) = client.recv().await? {
            if ev == ServerEvent::Ack {
                return anyhow::Ok(());
            }
        }
        anyhow::Ok(())
    })
    .await;
    match acked {
        Ok(result) => result?,
        Err(_) => anyhow::bail!(
            "the daemon did not answer within {} s",
            answer_within.as_secs_f32()
        ),
    }
    println!("termist: daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
