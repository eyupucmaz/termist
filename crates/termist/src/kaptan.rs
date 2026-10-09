//! `termist spawn`, `termist worktree` and `termist open`: what an agent (or you, from a
//! terminal) asks of termist.
use anyhow::{Context, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use termist_core::{AgentStatus, ClientRequest, Harness, ServerEvent, SessionId};
use termist_platform::{Client, Paths};

/// `termist spawn`'s words.
pub struct SpawnArgs {
    pub task: String,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub preset: Option<String>,
    pub worktree: Option<String>,
    pub wait: bool,
}

/// The card this runs in, when termist started it.
fn caller() -> Option<SessionId> {
    std::env::var("TERMIST_SESSION_ID").ok()?.parse().ok()
}

/// Inside a card its daemon runs; from a terminal one is started when there is none.
async fn connect(paths: &Paths) -> anyhow::Result<Client> {
    match caller() {
        Some(_) => Client::connect(paths).await,
        None => termist_tui::run::connect_or_spawn(paths).await,
    }
}

async fn next(c: &mut Client) -> anyhow::Result<ServerEvent> {
    c.recv().await?.ok_or_else(|| anyhow!("lost the daemon"))
}

/// How long the daemon may take to answer (a worktree may be fetched first); `--wait`
/// waits as long as the agent works.
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

/// `ask`'s answer, or why there was none in time.
async fn answered<T>(
    ask: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(ANSWER_WITHIN, ask)
        .await
        .map_err(|_| {
            anyhow!(
                "the daemon did not answer within {} s",
                ANSWER_WITHIN.as_secs()
            )
        })?
}

/// What `--wait` says of a status, and the exit code; `None` while it works.
fn outcome(status: AgentStatus) -> Option<(&'static str, u8)> {
    match status {
        AgentStatus::Unseen | AgentStatus::Finished => Some(("done", 0)),
        AgentStatus::NeedsFeedback => Some(("waiting for you", 2)),
        AgentStatus::Exited { .. } | AgentStatus::Disconnected => Some(("exited", 1)),
        AgentStatus::Fresh | AgentStatus::Running => None,
    }
}

pub async fn spawn(paths: &Paths, a: SpawnArgs) -> anyhow::Result<ExitCode> {
    let (mut harness, mut model, mut effort, mut around) = (a.harness, a.model, a.effort, None);
    if let Some(name) = &a.preset {
        let (config, _) = termist_platform::config_file::load(paths);
        let p = config
            .presets
            .iter()
            .find(|p| &p.name == name)
            .ok_or_else(|| anyhow!("there is no preset {name}; `e` in termist lists them"))?;
        harness = harness.or(Some(p.harness));
        model = model.or_else(|| p.model.clone());
        effort = effort.or_else(|| p.effort.clone());
        if !p.prefix.is_empty() || !p.postfix.is_empty() {
            around = Some((p.prefix.clone(), p.postfix.clone()));
        }
    }
    let mut c = connect(paths).await?;
    c.send(&ClientRequest::Spawn {
        ticket: 1,
        from: caller(),
        cwd: std::env::current_dir().context("this folder")?,
        task: a.task,
        harness,
        model,
        effort,
        worktree: a.worktree,
        around,
    })
    .await?;
    let session = answered(async {
        loop {
            match next(&mut c).await? {
                ServerEvent::Spawned {
                    ticket: 1,
                    session,
                    name,
                    harness,
                    place,
                } => {
                    println!("started {name} ({}) in {place}", harness.id());
                    return Ok(session);
                }
                ServerEvent::SpawnFailed { ticket: 1, message } => bail!(message),
                _ => {}
            }
        }
    })
    .await?;
    if !a.wait {
        return Ok(ExitCode::SUCCESS);
    }
    loop {
        match next(&mut c).await? {
            ServerEvent::SessionUpdated(info) if info.id == session => {
                if let Some((word, code)) = outcome(info.status) {
                    println!("{word}");
                    return Ok(ExitCode::from(code));
                }
            }
            ServerEvent::SessionRemoved(id) if id == session => {
                println!("exited");
                return Ok(ExitCode::from(1));
            }
            _ => {}
        }
    }
}

pub async fn worktree(paths: &Paths, branch: String) -> anyhow::Result<ExitCode> {
    let session = caller();
    let mut c = connect(paths).await?;
    c.send(&ClientRequest::MoveSession {
        ticket: 1,
        session,
        cwd: std::env::current_dir().context("this folder")?,
        branch,
    })
    .await?;
    answered(async {
        loop {
            match next(&mut c).await? {
                ServerEvent::Moved {
                    ticket: 1,
                    path,
                    branch,
                    new_from,
                } => {
                    let how = match new_from {
                        Some(base) => format!("branch {branch}, new from {base}"),
                        None => format!("branch {branch}"),
                    };
                    match session {
                        Some(_) => println!(
                            "moved to {} ({how}); work there: cd into it in each command, or use \
                         absolute paths",
                            path.display()
                        ),
                        None => println!("{} ({how})", path.display()),
                    }
                    return Ok(ExitCode::SUCCESS);
                }
                ServerEvent::MoveFailed { ticket: 1, message } => bail!(message),
                _ => {}
            }
        }
    })
    .await
}

/// `file:line`: the line when what follows the last `:` is a number.
pub fn split_line(target: &str) -> (&str, Option<u32>) {
    match target.rsplit_once(':') {
        Some((file, line)) if !file.is_empty() => match line.parse() {
            Ok(n) => (file, Some(n)),
            Err(_) => (target, None),
        },
        _ => (target, None),
    }
}

/// The folder to open and the file in it: this folder and the file's path in it, or,
/// for a file elsewhere, its own folder.
fn folder_and_file(here: &Path, path: &Path) -> (PathBuf, Option<String>) {
    if path.is_dir() {
        return (path.to_path_buf(), None);
    }
    let folder = if path.starts_with(here) {
        here.to_path_buf()
    } else {
        path.parent().unwrap_or(here).to_path_buf()
    };
    let file = path
        .strip_prefix(&folder)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    (folder, Some(file))
}

pub async fn open(paths: &Paths, target: &str) -> anyhow::Result<ExitCode> {
    let (file, line) = split_line(target);
    let here = std::env::current_dir().context("this folder")?;
    let path = here.join(file);
    if !path.exists() {
        bail!("{} is not there", path.display());
    }
    let (folder, file) = folder_and_file(&here, &path);
    let mut c = connect(paths).await?;
    c.send(&ClientRequest::ListState).await?;
    let state = answered(async {
        loop {
            if let ServerEvent::State(state) = next(&mut c).await? {
                return Ok(state);
            }
        }
    })
    .await?;
    let mine = caller().and_then(|id| state.sessions.iter().find(|s| s.id == id));
    let project = match mine {
        Some(s) => s.project,
        None => {
            let at = std::fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
            state
                .projects
                .iter()
                .filter(|p| p.open && at.starts_with(&p.path))
                .max_by_key(|p| p.path.as_os_str().len())
                .map(|p| p.id)
                .ok_or_else(|| {
                    anyhow!(
                        "{} is not in a termist project; open its folder in termist first",
                        folder.display()
                    )
                })?
        }
    };
    let (config, _) = termist_platform::config_file::load(paths);
    let editor = config
        .editor
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|e| !e.trim().is_empty());
    c.send(&ClientRequest::OpenInEditor {
        project,
        folder,
        file,
        line,
        editor,
    })
    .await?;
    // An editor that cannot be started is said before the answer to this.
    c.send(&ClientRequest::ListState).await?;
    answered(async {
        loop {
            match next(&mut c).await? {
                ServerEvent::EditorFailed { message } => bail!(message),
                ServerEvent::State(_) => return Ok(ExitCode::SUCCESS),
                _ => {}
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_follows_the_last_colon_when_it_is_a_number() {
        assert_eq!(split_line("src/main.rs:42"), ("src/main.rs", Some(42)));
        assert_eq!(split_line("src/main.rs"), ("src/main.rs", None));
        assert_eq!(split_line(r"C:\work\a.rs"), (r"C:\work\a.rs", None));
        assert_eq!(split_line(":3"), (":3", None));
    }

    #[test]
    fn a_file_here_opens_this_folder_and_one_elsewhere_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (here, other) = (tmp.path().join("site"), tmp.path().join("docs"));
        std::fs::create_dir_all(here.join("src")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(here.join("src").join("a.rs"), "").unwrap();
        std::fs::write(other.join("b.md"), "").unwrap();
        assert_eq!(
            folder_and_file(&here, &here.join("src").join("a.rs")),
            (
                here.clone(),
                Some(format!("src{}a.rs", std::path::MAIN_SEPARATOR))
            )
        );
        assert_eq!(
            folder_and_file(&here, &other.join("b.md")),
            (other.clone(), Some("b.md".into()))
        );
        assert_eq!(folder_and_file(&here, &other), (other, None));
    }

    #[test]
    fn waiting_ends_when_the_agent_is_done_waits_for_you_or_stopped() {
        assert_eq!(outcome(AgentStatus::Running), None);
        assert_eq!(outcome(AgentStatus::Unseen), Some(("done", 0)));
        assert_eq!(
            outcome(AgentStatus::NeedsFeedback),
            Some(("waiting for you", 2))
        );
        assert_eq!(
            outcome(AgentStatus::Exited { code: Some(1) }),
            Some(("exited", 1))
        );
    }
}
