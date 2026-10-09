//! Kaptan in the registry: an agent starts another agent (`termist spawn`) or goes on in
//! a worktree (`termist worktree`), through termist; a worktree it needs is made on a
//! blocking thread first.
use super::*;

/// A `Spawn` as asked.
#[derive(Debug)]
pub struct Asked {
    pub ticket: u64,
    pub from: Option<SessionId>,
    pub cwd: PathBuf,
    pub task: String,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub worktree: Option<String>,
    pub around: Option<(String, String)>,
}

/// An agent to start once its folder is known.
#[derive(Debug)]
pub struct Start {
    client: ClientId,
    ticket: u64,
    from: Option<SessionId>,
    project: ProjectId,
    folder: PathBuf,
    /// The worktree's branch, when it starts in one made or found for it.
    branch: Option<String>,
    harness: Harness,
    model: Option<String>,
    effort: Option<String>,
    prompt: String,
    title_from: Option<String>,
}

/// What waits for a worktree.
#[derive(Debug)]
pub enum After {
    Spawn(Box<Start>),
}

/// A worktree made (or found) for an agent's request, on a blocking thread.
pub struct Ready {
    pub after: After,
    /// The repo's main folder.
    pub repo: PathBuf,
    pub result: Result<crate::worktrees::Made, String>,
}

/// "1 agent", "2 agents".
fn agents(n: usize) -> String {
    match n {
        1 => "1 agent".into(),
        n => format!("{n} agents"),
    }
}

impl Registry {
    /// `termist spawn`: where the agent starts (the asking card's folder, or a folder of
    /// a project), whether the asking agent may start one more, and its worktree.
    pub(super) fn spawn(&mut self, client: ClientId, asked: Asked) {
        let ticket = asked.ticket;
        if let Err(message) = self.spawn_or_wait(client, asked) {
            self.send(client, ServerEvent::SpawnFailed { ticket, message });
        }
    }

    fn spawn_or_wait(&mut self, client: ClientId, asked: Asked) -> Result<(), String> {
        let Asked {
            ticket,
            from,
            cwd,
            task,
            harness,
            model,
            effort,
            worktree,
            around,
        } = asked;
        let task = task.trim().to_string();
        if task.is_empty() {
            return Err("a task is needed: termist spawn \"<task>\"".into());
        }
        let (project, folder, theirs) = match from {
            Some(by) => {
                let s = self.session(by).ok_or("the card that asked is gone")?;
                let theirs = match s.info.kind {
                    SessionKind::Agent { harness } => Some(harness),
                    _ => None,
                };
                (s.info.project, s.info.cwd.clone(), theirs)
            }
            None => (self.project_of(&cwd)?, cwd, None),
        };
        if let Some(by) = from {
            self.may_start_one_more(by)?;
        }
        let harness = harness
            .or(theirs)
            .or(self.last_launch.as_ref().map(|l| l.harness))
            .unwrap_or_else(|| self.agents_config().default);
        let prompt = match &around {
            Some((before, after)) => format!("{before}{task}{after}"),
            None => task.clone(),
        };
        let start = Start {
            client,
            ticket,
            from,
            project,
            folder,
            branch: None,
            harness,
            model,
            effort,
            prompt,
            title_from: around.is_some().then_some(task),
        };
        match worktree {
            None => self.start_spawned(start),
            Some(branch) => {
                self.make_worktree(&start.folder.clone(), branch, After::Spawn(Box::new(start)))?
            }
        }
        Ok(())
    }

    /// An agent started by an agent starts none; an agent starts at most
    /// `[agents] max_spawned` that run at once.
    fn may_start_one_more(&self, by: SessionId) -> Result<(), String> {
        let s = self.session(by).ok_or("the card that asked is gone")?;
        let name = s.info.display_name();
        if s.spawned_by.is_some() {
            return Err(format!(
                "{name} was started by an agent; it cannot start another"
            ));
        }
        let running = self
            .sessions
            .iter()
            .filter(|c| c.spawned_by == Some(by) && c.info.status.is_live())
            .count();
        if running >= self.agents_config().max_spawned as usize {
            return Err(format!(
                "{name} has {} running; wait for one to finish",
                agents(running)
            ));
        }
        Ok(())
    }

    /// The open project `dir` is in, or whose repo it is a worktree of; the deepest
    /// when projects nest.
    fn project_of(&self, dir: &Path) -> Result<ProjectId, String> {
        let here = place::resolved(dir);
        let main = place::repo_top(dir).map(|top| place::resolved(&place::main_of(&top)));
        let found = self
            .projects
            .iter()
            .filter(|p| {
                let root = place::resolved(&p.path);
                here.starts_with(&root) || main.as_ref().is_some_and(|m| m.starts_with(&root))
            })
            .max_by_key(|p| p.path.as_os_str().len());
        match found {
            None => Err(format!(
                "{} is not in a termist project; open its folder in termist first",
                dir.display()
            )),
            Some(p) if !p.open => Err(format!("{} is closed in termist; open it first", p.name)),
            Some(p) => Ok(p.id),
        }
    }

    /// Makes (or finds) `branch`'s worktree of the repo `folder` is in, on a blocking
    /// thread; `worktree_ready` goes on.
    fn make_worktree(&mut self, folder: &Path, branch: String, after: After) -> Result<(), String> {
        let top = place::repo_top(folder).ok_or("not in a git repo")?;
        let repo = place::main_of(&top);
        let (tx, fetch, git) = (
            self.ready_tx.clone(),
            self.fetch,
            crate::github::worktree::git,
        );
        tokio::task::spawn_blocking(move || {
            let result = crate::worktrees::create(&repo, &branch, &git, &fetch);
            let _ = tx.send(Ready {
                after,
                repo,
                result,
            });
        });
        Ok(())
    }

    /// The worktree is there: kept as termist's, and what waited for it goes on.
    pub fn worktree_ready(&mut self, ready: Ready) {
        let Ready {
            after,
            repo,
            result,
        } = ready;
        match after {
            After::Spawn(mut start) => match result {
                Ok(made) => {
                    let id = self.repo_id(start.project, &repo);
                    self.keep_worktree(start.project, id, &repo, &made);
                    self.send_worktrees(start.project);
                    self.scan_worktrees();
                    start.folder = made.path;
                    start.branch = Some(made.branch);
                    self.start_spawned(*start);
                }
                Err(why) => self.send(
                    start.client,
                    ServerEvent::SpawnFailed {
                        ticket: start.ticket,
                        message: format!("couldn't make a worktree · {why}"),
                    },
                ),
            },
        }
    }

    /// The project's GitHub repo whose main folder is `repo`, when it is one.
    fn repo_id(&self, project: ProjectId, repo: &Path) -> Option<termist_core::github::RepoId> {
        let main = place::resolved(repo);
        self.github
            .repo_views(project)
            .into_iter()
            .find(|v| v.main == main || v.path == main)
            .map(|v| v.id)
    }

    fn start_spawned(&mut self, start: Start) {
        let Start {
            client,
            ticket,
            from,
            project,
            folder,
            branch,
            harness,
            model,
            effort,
            prompt,
            title_from,
        } = start;
        let new = NewSession {
            project,
            kind: SessionKind::Agent { harness },
            cwd: Some(folder.clone()),
            prompt: Some(prompt),
            title_from,
            model,
            effort,
            size: (80, 24),
            spawned_by: from,
        };
        let id = match self.create_session_named(new) {
            Ok(id) => id,
            Err(e) => {
                let message = format!("{e:#}");
                self.send(client, ServerEvent::SpawnFailed { ticket, message });
                return;
            }
        };
        let name = self
            .session(id)
            .map(|s| s.info.display_name().to_string())
            .unwrap_or_default();
        let place = branch.unwrap_or_else(|| self.place_label(project, &folder));
        self.send(
            client,
            ServerEvent::Spawned {
                ticket,
                session: id,
                name: name.clone(),
                harness,
                place,
            },
        );
        if let Some(by) = from.and_then(|by| self.session(by)) {
            let text = format!("{} started {name}", by.info.display_name());
            self.broadcast(ServerEvent::Notice { text });
        }
    }

    /// Where a folder is, in words: the project's name inside its folder, else the
    /// branch of the worktree it is (as last read), else its name.
    fn place_label(&self, project: ProjectId, folder: &Path) -> String {
        let Some(p) = self.projects.iter().find(|p| p.id == project) else {
            return String::new();
        };
        if place::resolved(folder).starts_with(place::resolved(&p.path)) {
            return p.name.clone();
        }
        self.git_facts
            .get(folder)
            .and_then(|f| f.as_ref()?.branch.clone())
            .or_else(|| folder.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::tests::{connect, registry_on, run_git};
    use super::*;

    /// A repo `site` as an open project, with a Codex card `Fix Login` in it.
    fn site() -> (tempfile::TempDir, Registry, ProjectInfo, SessionId) {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir_all(site.join("src")).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        run_git(&site, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: site,
            open: true,
        };
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let mut reg = registry_on(store);
        reg.launcher.programs.codex = "true".into();
        reg.launcher.programs.claude = "true".into();
        reg.fetch = |_, _| Err("offline".into());
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::CreateSession {
                project: p.id,
                kind: SessionKind::Agent {
                    harness: Harness::Codex,
                },
                cwd: None,
                prompt: Some("fix the login".into()),
                title_from: None,
                model: None,
                effort: None,
                cols: 80,
                rows: 24,
            },
        });
        let id = reg.sessions[0].info.id;
        (tmp, reg, p, id)
    }

    fn ask(reg: &mut Registry, ticket: u64, from: Option<SessionId>, cwd: &Path, task: &str) {
        ask_in(reg, ticket, from, cwd, task, None, None);
    }

    fn ask_in(
        reg: &mut Registry,
        ticket: u64,
        from: Option<SessionId>,
        cwd: &Path,
        task: &str,
        worktree: Option<&str>,
        around: Option<(&str, &str)>,
    ) {
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::Spawn {
                ticket,
                from,
                cwd: cwd.to_path_buf(),
                task: task.into(),
                harness: None,
                model: None,
                effort: None,
                worktree: worktree.map(str::to_string),
                around: around.map(|(a, b)| (a.to_string(), b.to_string())),
            },
        });
    }

    fn drain(rx: &mut UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn failed(events: &[ServerEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                ServerEvent::SpawnFailed { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_agent_starts_an_agent_beside_it_and_every_client_hears_of_it() {
        let (_tmp, mut reg, p, fix) = site();
        let mut rx = connect(&mut reg);
        ask(
            &mut reg,
            1,
            Some(fix),
            Path::new("/ignored"),
            "write the tests",
        );
        let events = drain(&mut rx);
        let new = reg.sessions[1].info.clone();
        assert_eq!(new.name, "Write Tests");
        assert_eq!(
            new.kind,
            SessionKind::Agent {
                harness: Harness::Codex
            },
            "its CLI"
        );
        assert_eq!(new.cwd, p.path, "beside the agent that asked");
        assert!(events.contains(&ServerEvent::Spawned {
            ticket: 1,
            session: new.id,
            name: "Write Tests".into(),
            harness: Harness::Codex,
            place: "site".into(),
        }));
        assert!(events.contains(&ServerEvent::Notice {
            text: "Fix Login started Write Tests".into()
        }));
        assert_eq!(reg.sessions[1].spawned_by, Some(fix));
        assert_eq!(reg.store.spawned_by().unwrap()[&new.id], fix, "kept");
        assert!(
            !reg.store
                .prompt_history(10)
                .unwrap()
                .contains(&"write the tests".to_string()),
            "an agent's task is not the user's prompt to recall"
        );
    }

    #[tokio::test]
    async fn an_agent_has_so_many_agents_running_and_theirs_start_none() {
        let (tmp, mut reg, _p, fix) = site();
        let paths = termist_platform::Paths::under(tmp.path().join("home"));
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_path(), "[agents]\nmax_spawned = 1\n").unwrap();
        reg.config_paths = Some(paths);
        let mut rx = connect(&mut reg);
        ask(&mut reg, 1, Some(fix), Path::new("/"), "write the tests");
        ask(&mut reg, 2, Some(fix), Path::new("/"), "update the docs");
        let child = reg.sessions[1].info.id;
        ask(&mut reg, 3, Some(child), Path::new("/"), "go deeper");
        assert_eq!(
            failed(&drain(&mut rx)),
            [
                "Fix Login has 1 agent running; wait for one to finish",
                "Write Tests was started by an agent; it cannot start another",
            ]
        );
        assert_eq!(reg.sessions.len(), 2);
        // One that ended makes room.
        reg.sessions[1].info.status = AgentStatus::Exited { code: Some(0) };
        ask(&mut reg, 4, Some(fix), Path::new("/"), "update the docs");
        assert_eq!(reg.sessions.len(), 3);
    }

    #[tokio::test]
    async fn from_outside_termist_a_folder_of_a_project_is_where_it_starts() {
        let (tmp, mut reg, p, _) = site();
        let mut rx = connect(&mut reg);
        ask_in(
            &mut reg,
            1,
            None,
            &p.path.join("src"),
            "fix login",
            None,
            Some(("Review: ", " then list")),
        );
        let events = drain(&mut rx);
        let new = reg.sessions[1].info.clone();
        assert_eq!(new.cwd, p.path.join("src"));
        assert_eq!(
            new.name, "Fix Login",
            "named after the task, not the preset's words"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ServerEvent::Notice { .. })),
            "no agent asked"
        );
        assert_eq!(
            reg.store.prompt_history(1).unwrap(),
            ["Review: fix login then list"]
        );
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        ask(&mut reg, 2, None, &elsewhere, "fix login");
        ask(&mut reg, 3, None, &p.path, "   ");
        let events = drain(&mut rx);
        let failed = failed(&events);
        assert!(
            failed[0].ends_with("is not in a termist project; open its folder in termist first"),
            "{failed:?}"
        );
        assert_eq!(failed[1], "a task is needed: termist spawn \"<task>\"");
    }

    #[tokio::test]
    async fn with_a_branch_the_agent_starts_in_its_worktree_beside_the_repo() {
        let (tmp, mut reg, _p, fix) = site();
        let mut ready = reg.ready_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        ask_in(
            &mut reg,
            1,
            Some(fix),
            Path::new("/"),
            "write the tests",
            Some("fix-login"),
            None,
        );
        assert!(
            drain(&mut rx).is_empty(),
            "not before the worktree is there"
        );
        let r = ready.recv().await.unwrap();
        reg.worktree_ready(r);
        let events = drain(&mut rx);
        let want = place::resolved(tmp.path())
            .join("site-worktrees")
            .join("fix-login");
        let new = reg.sessions[1].info.clone();
        assert_eq!(place::resolved(&new.cwd), want);
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::Spawned { place, .. } if place == "fix-login"
        )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::Worktrees { .. }))
        );
        // The branch's worktree again: the same folder.
        ask_in(
            &mut reg,
            2,
            Some(fix),
            Path::new("/"),
            "review it",
            Some("fix-login"),
            None,
        );
        let r = ready.recv().await.unwrap();
        reg.worktree_ready(r);
        assert_eq!(place::resolved(&reg.sessions[2].info.cwd), want);
        // A project that is no repo has no worktrees.
        let notes = tmp.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        reg.projects.push(ProjectInfo {
            id: ProjectId::new(),
            name: "notes".into(),
            path: notes.clone(),
            open: true,
        });
        drain(&mut rx);
        ask_in(&mut reg, 3, None, &notes, "tidy", Some("x"), None);
        assert_eq!(failed(&drain(&mut rx)), ["not in a git repo"]);
    }
}
