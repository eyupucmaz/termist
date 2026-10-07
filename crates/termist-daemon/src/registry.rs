use crate::github::gh::{CliGh, GhHandle};
use crate::github::jobs::Locate;
use crate::github::{self, Done, Effects, GitHub, To};
use crate::launch::{LaunchRequest, Launcher};
use crate::place::{self, GitFacts};
use crate::session::{self, ClientId, SessionCmd, SessionNote};
use crate::store::{PROMPT_HISTORY_MAX, Store, StoredSession, StoredWorktree};
use crate::transcript::TranscriptTail;
use crate::worktrees::Listed;
use crate::{claude, codex, opencode};
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use termist_core::{
    AgentStatus, ClientRequest, Harness, HarnessInfo, LaunchOptions, ModelInfo, ProjectId,
    ProjectInfo, ServerEvent, SessionId, SessionInfo, SessionKind, Signal, StateSnapshot,
    TermColors, now_ms,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

/// How often a session's PTY activity is broadcast to clients as `SessionUpdated`.
const ACTIVITY_BROADCAST: Duration = Duration::from_secs(5);

/// A running Claude card whose title has been idle this long, with no hook in between,
/// was cancelled before its answer started (Claude sends no hook for that).
const IDLE_TITLE_CANCEL: Duration = Duration::from_millis(1500);

/// How often the sessions' folders are asked again which branch they are on: an agent
/// may check out another one.
const PLACES_ROUND: Duration = Duration::from_secs(30);

/// What git said about a session folder, read on a blocking thread.
pub struct PlaceRead(pub PathBuf, pub Option<GitFacts>);

/// A repo's worktrees as a scan found them, each with what its branch changed; `None`
/// when git could not list them.
pub struct WorktreeScan {
    pub project: ProjectId,
    pub repo: termist_core::github::RepoId,
    pub found: Option<Vec<(Listed, Option<termist_core::Stat>)>>,
}

/// A worktree made (or not) for a client's `CreateWorktree`.
pub struct Made {
    pub client: ClientId,
    pub ticket: u64,
    pub project: ProjectId,
    pub repo: termist_core::github::RepoId,
    pub branch: String,
    /// The folder, its base branch and a note; or why not.
    pub result: Result<(PathBuf, String, Option<String>), String>,
    /// The repo's own folder: a branch open there is not a worktree to keep.
    pub own: PathBuf,
}

/// A worktree removed (or not) for a client's `RemoveWorktree`: `Ok(Some(files))`
/// when it has uncommitted changes and was left.
pub struct Removed {
    pub client: ClientId,
    pub project: ProjectId,
    pub path: PathBuf,
    pub result: Result<Option<u32>, String>,
}

/// A rescan for missing CLIs can start a login shell; one per this window is enough.
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);

/// The agent CLIs a rescan found, with the program to launch for each.
#[derive(Debug)]
pub struct Rescanned(pub Vec<(Harness, PathBuf)>);

/// A catalog read on a blocking thread, for `catalog_read`.
pub struct Catalog(pub Harness, pub Vec<ModelInfo>);

/// A CLI's model list is asked for again after this long.
pub const CATALOG_FRESH: Duration = Duration::from_secs(60 * 60);

pub enum Msg {
    Connected {
        client: ClientId,
        out: UnboundedSender<ServerEvent>,
    },
    Disconnected(ClientId),
    Request {
        client: ClientId,
        req: ClientRequest,
    },
}

struct Session {
    info: SessionInfo,
    /// `None` for a session loaded from the store that has not been resumed yet.
    cmd: Option<UnboundedSender<SessionCmd>>,
    transcript: Option<TranscriptTail>,
    /// Last time an `Activity` note was broadcast; throttles `SessionUpdated`.
    activity_broadcast: Option<std::time::Instant>,
    /// The agent's conversation exists, so Resume may pass its id. Claude's id is
    /// assigned up front, but Claude only creates the conversation with the first prompt.
    resumable: bool,
    /// OpenCode subagent session ids seen for this card; their events are ignored.
    opencode_children: HashSet<String>,
    /// Claude: since when the title has shown idle while the card is running.
    idle_title_since: Option<std::time::Instant>,
    /// Claude: the card is `Finished` only because its title looked idle. That is a
    /// guess; a turn that goes on after it undoes it.
    title_cancelled: bool,
}

impl Session {
    fn new(
        info: SessionInfo,
        cmd: Option<UnboundedSender<SessionCmd>>,
        resumable: bool,
    ) -> Session {
        Session {
            info,
            cmd,
            transcript: None,
            activity_broadcast: None,
            resumable,
            opencode_children: HashSet::new(),
            idle_title_since: None,
            title_cancelled: false,
        }
    }
}

pub struct Registry {
    launcher: Launcher,
    harnesses: Vec<HarnessInfo>,
    store: Store,
    projects: Vec<ProjectInfo>,
    sessions: Vec<Session>,
    /// The quick prompt's last choice, as the store has it.
    last_launch: Option<LaunchOptions>,
    /// What agents are told their terminal's colours are: the last client's.
    colors: TermColors,
    clients: HashMap<ClientId, UnboundedSender<ServerEvent>>,
    notes: UnboundedSender<SessionNote>,
    shutdown: Option<oneshot::Sender<()>>,
    created: u32,
    /// A rescan runs on a blocking thread and reports back through this channel;
    /// `run` takes the receiving end.
    rescans: UnboundedSender<Rescanned>,
    rescans_rx: Option<UnboundedReceiver<Rescanned>>,
    rescanning: bool,
    /// When the last rescan started.
    last_rescan: Option<std::time::Instant>,
    /// Looks a CLI up (PATH, then the login shell).
    find_program: fn(&str) -> Option<PathBuf>,
    /// The models each CLI offers, and when they were read.
    catalogs: HashMap<Harness, (Vec<ModelInfo>, std::time::Instant)>,
    /// Clients that asked while a CLI's list was being read; each gets it when it comes.
    catalog_waiters: HashMap<Harness, Vec<ClientId>>,
    catalog_tx: UnboundedSender<Catalog>,
    catalog_rx: Option<UnboundedReceiver<Catalog>>,
    /// Asks a CLI for its models (a stand-in in tests).
    read_catalog: fn(Harness, &str) -> Vec<ModelInfo>,
    /// Pull requests; its jobs run on blocking threads and report on `github_tx`.
    github: GitHub,
    github_tx: UnboundedSender<Done>,
    github_rx: Option<UnboundedReceiver<Done>>,
    /// Finds gh for those jobs.
    locate: Locate,
    /// What git said about each session folder; `None` when it is not a repo.
    git_facts: HashMap<PathBuf, Option<GitFacts>>,
    places_tx: UnboundedSender<PlaceRead>,
    places_rx: Option<UnboundedReceiver<PlaceRead>>,
    places_reading: HashSet<PathBuf>,
    /// When the folders were last asked all at once.
    places_round: Option<std::time::Instant>,
    /// Runs git (a stand-in in tests).
    git: fn(&Path, &[&str]) -> Option<String>,
    scans_tx: UnboundedSender<WorktreeScan>,
    scans_rx: Option<UnboundedReceiver<WorktreeScan>>,
    /// Repos whose worktrees are being read.
    scanning: HashSet<termist_core::github::RepoId>,
    /// What each worktree's branch changed, as last read.
    worktree_stats: HashMap<PathBuf, termist_core::Stat>,
    /// Each project's worktrees as last sent.
    worktrees_sent: HashMap<ProjectId, Vec<termist_core::WorktreeInfo>>,
    made_tx: UnboundedSender<Made>,
    made_rx: Option<UnboundedReceiver<Made>>,
    /// Fetches before a new branch is made (a stand-in in tests).
    fetch: fn(&Path, &[&str]) -> Result<String, String>,
    removed_tx: UnboundedSender<Removed>,
    removed_rx: Option<UnboundedReceiver<Removed>>,
}

impl Registry {
    pub fn new(
        launcher: Launcher,
        harnesses: Vec<HarnessInfo>,
        store: Store,
        notes: UnboundedSender<SessionNote>,
        shutdown: oneshot::Sender<()>,
    ) -> Registry {
        let (projects, mut sessions) = store.load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not load stored sessions");
            (vec![], vec![])
        });
        // Stored before sessions had folders: they ran in their project's.
        for s in sessions
            .iter_mut()
            .filter(|s| s.info.cwd.as_os_str().is_empty())
        {
            if let Some(p) = projects.iter().find(|p| p.id == s.info.project) {
                s.info.cwd = p.path.clone();
            }
        }
        // Names are `<label>-<n>`: go on from the highest n, so none repeats.
        let created = sessions
            .iter()
            .filter_map(|s| s.info.name.rsplit_once('-')?.1.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        let last_launch = store.last_launch();
        let (rescans, rescans_rx) = tokio::sync::mpsc::unbounded_channel();
        let (catalog_tx, catalog_rx) = tokio::sync::mpsc::unbounded_channel();
        let github = GitHub::new(&store);
        let (github_tx, github_rx) = tokio::sync::mpsc::unbounded_channel();
        let (places_tx, places_rx) = tokio::sync::mpsc::unbounded_channel();
        let (scans_tx, scans_rx) = tokio::sync::mpsc::unbounded_channel();
        let (made_tx, made_rx) = tokio::sync::mpsc::unbounded_channel();
        let (removed_tx, removed_rx) = tokio::sync::mpsc::unbounded_channel();
        let gh_bin = launcher.config.gh_bin.clone();
        let locate: Locate = Arc::new(move || {
            let program = gh_bin
                .clone()
                .map(PathBuf::from)
                .or_else(|| crate::resolve::find_program("gh"))?;
            Some(GhHandle(Arc::new(CliGh { program })))
        });
        Registry {
            launcher,
            harnesses,
            store,
            projects,
            sessions: sessions
                .into_iter()
                .map(|StoredSession { info, resumable }| Session::new(info, None, resumable))
                .collect(),
            last_launch,
            colors: TermColors::default(),
            clients: HashMap::new(),
            notes,
            shutdown: Some(shutdown),
            created,
            rescans,
            rescans_rx: Some(rescans_rx),
            rescanning: false,
            last_rescan: None,
            find_program: crate::resolve::find_program,
            catalogs: HashMap::new(),
            catalog_waiters: HashMap::new(),
            catalog_tx,
            catalog_rx: Some(catalog_rx),
            read_catalog: crate::models::catalog,
            github,
            github_tx,
            github_rx: Some(github_rx),
            locate,
            git_facts: HashMap::new(),
            places_tx,
            places_rx: Some(places_rx),
            places_reading: HashSet::new(),
            places_round: None,
            git: crate::github::repos::git,
            scans_tx,
            scans_rx: Some(scans_rx),
            scanning: HashSet::new(),
            worktree_stats: HashMap::new(),
            worktrees_sent: HashMap::new(),
            made_tx,
            made_rx: Some(made_rx),
            fetch: crate::worktrees::fetch,
            removed_tx,
            removed_rx: Some(removed_rx),
        }
    }

    /// Asks git about a session folder on a blocking thread; `place_read` takes the answer.
    fn read_place(&mut self, cwd: PathBuf) {
        if !self.places_reading.insert(cwd.clone()) {
            return;
        }
        let (tx, git) = (self.places_tx.clone(), self.git);
        tokio::task::spawn_blocking(move || {
            let facts = place::read(&cwd, &git);
            let _ = tx.send(PlaceRead(cwd, facts));
        });
    }

    pub fn place_read(&mut self, PlaceRead(cwd, facts): PlaceRead) {
        self.places_reading.remove(&cwd);
        self.git_facts.insert(cwd.clone(), facts);
        self.refresh_places(Some(&cwd));
    }

    /// Every session folder is asked again now and then while someone looks.
    pub fn places_tick(&mut self, now: std::time::Instant) {
        if self.clients.is_empty()
            || self
                .places_round
                .is_some_and(|t| now.saturating_duration_since(t) < PLACES_ROUND)
        {
            return;
        }
        self.places_round = Some(now);
        let cwds: HashSet<PathBuf> = self
            .sessions
            .iter()
            .filter(|s| !s.info.archived)
            .map(|s| s.info.cwd.clone())
            .collect();
        for cwd in cwds {
            self.read_place(cwd);
        }
        self.scan_worktrees();
    }

    /// Reads every open project's repos' worktrees on blocking threads; `worktrees_scanned`
    /// takes each answer.
    fn scan_worktrees(&mut self) {
        let kept = self.store.worktrees().unwrap_or_default();
        // The pull requests kept worktrees were on: how the ones no longer open ended.
        let prs: Vec<termist_core::github::PrRef> = kept
            .iter()
            .filter_map(|w| {
                Some(termist_core::github::PrRef {
                    repo: w.repo?,
                    number: w.pr?,
                })
            })
            .collect();
        let fx = self.github.ask_pr_ends(&prs);
        self.github_effects(fx);
        let bases: HashMap<PathBuf, String> = kept
            .into_iter()
            .filter_map(|w| Some((w.path, w.base?)))
            .collect();
        for p in self.projects.iter().filter(|p| p.open) {
            for view in self.github.repo_views(p.id) {
                if !self.scanning.insert(view.id) {
                    continue;
                }
                let (tx, bases, own) = (self.scans_tx.clone(), bases.clone(), p.path.clone());
                let (project, repo, path) = (p.id, view.id, view.path.clone());
                tokio::task::spawn_blocking(move || {
                    let base = |w: &Path| bases.get(w).cloned();
                    let found =
                        crate::worktrees::scan(&path, &own, &base, &crate::github::worktree::git);
                    let _ = tx.send(WorktreeScan {
                        project,
                        repo,
                        found,
                    });
                });
            }
        }
    }

    /// Keeps what a scan found: new worktrees are added (shown when an open pull request
    /// has their branch), ones git no longer lists are dropped.
    pub fn worktrees_scanned(&mut self, scan: WorktreeScan) {
        self.scanning.remove(&scan.repo);
        let Some(found) = scan.found else {
            return;
        };
        let kept: Vec<StoredWorktree> = self
            .store
            .worktrees()
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.project == scan.project && w.repo == Some(scan.repo))
            .collect();
        let open: HashSet<String> = self
            .github
            .repo_views(scan.project)
            .into_iter()
            .filter(|v| v.id == scan.repo)
            .flat_map(|v| v.prs.into_iter().map(|(_, head, _)| head))
            .collect();
        for (listed, stat) in &found {
            let known = kept.iter().any(|w| w.path == listed.path);
            let fresh = StoredWorktree {
                project: scan.project,
                repo: Some(scan.repo),
                path: listed.path.clone(),
                branch: listed.branch.clone(),
                base: None,
                pr: None,
                made_by_termist: false,
                shown: !known && listed.branch.as_ref().is_some_and(|b| open.contains(b)),
            };
            if let Err(e) = self.store.upsert_worktree(&fresh) {
                tracing::warn!(error = %e, "could not keep a worktree");
            }
            match stat {
                Some(stat) => self.worktree_stats.insert(listed.path.clone(), *stat),
                None => self.worktree_stats.remove(&listed.path),
            };
        }
        for gone in kept
            .iter()
            .filter(|w| !found.iter().any(|(l, _)| l.path == w.path))
        {
            if let Err(e) = self.store.delete_worktree(&gone.path) {
                tracing::warn!(error = %e, "could not forget a worktree");
            }
            self.worktree_stats.remove(&gone.path);
        }
        self.send_worktrees(scan.project);
    }

    /// Makes a worktree on a blocking thread; `worktree_made` answers the client.
    fn create_worktree(
        &mut self,
        client: ClientId,
        project: ProjectId,
        repo: termist_core::github::RepoId,
        branch: String,
        ticket: u64,
    ) {
        let Some(view) = self
            .github
            .repo_views(project)
            .into_iter()
            .find(|v| v.id == repo)
        else {
            self.send(
                client,
                ServerEvent::WorktreeNotMade {
                    ticket,
                    message: "couldn't make a worktree · not loaded yet".into(),
                },
            );
            return;
        };
        let (tx, fetch) = (self.made_tx.clone(), self.fetch);
        tokio::task::spawn_blocking(move || {
            let result = crate::worktrees::create(
                &view.path,
                &branch,
                &crate::github::worktree::git,
                &fetch,
            );
            let _ = tx.send(Made {
                client,
                ticket,
                project,
                repo,
                branch,
                result,
                own: view.path,
            });
        });
    }

    /// Keeps the worktree made, tells the client, and reads the worktrees again.
    pub fn worktree_made(&mut self, m: Made) {
        match m.result {
            Ok((path, base, note)) => {
                // A branch already open in the repo's own folder is no worktree to keep.
                if place::resolved(&path) != place::resolved(&m.own) {
                    let made = StoredWorktree {
                        project: m.project,
                        repo: Some(m.repo),
                        path: path.clone(),
                        branch: Some(m.branch.clone()),
                        base: Some(base),
                        pr: None,
                        made_by_termist: true,
                        shown: true,
                    };
                    if let Err(e) = self.store.upsert_worktree(&made) {
                        tracing::warn!(error = %e, "could not keep a worktree");
                    }
                }
                self.send(
                    m.client,
                    ServerEvent::WorktreeMade {
                        ticket: m.ticket,
                        path,
                        branch: m.branch,
                        note,
                    },
                );
                self.send_worktrees(m.project);
                self.scan_worktrees();
            }
            Err(why) => self.send(
                m.client,
                ServerEvent::WorktreeNotMade {
                    ticket: m.ticket,
                    message: format!("couldn't make a worktree · {why}"),
                },
            ),
        }
    }

    /// Removes a worktree termist keeps, on a blocking thread: never a repo's or a
    /// project's own folder, never one a live card runs in.
    fn remove_worktree(
        &mut self,
        client: ClientId,
        path: PathBuf,
        force: bool,
    ) -> Result<(), String> {
        let kept = self.store.worktrees().unwrap_or_default();
        let w = kept
            .iter()
            .find(|w| w.path == path)
            .ok_or("not a worktree termist knows of")?;
        let view = self
            .github
            .repo_views(w.project)
            .into_iter()
            .find(|v| Some(v.id) == w.repo)
            .ok_or("its repo is not loaded yet")?;
        let here = place::resolved(&path);
        let own = self
            .projects
            .iter()
            .any(|p| place::resolved(&p.path) == here)
            || place::resolved(&view.path) == here
            || view.main == here;
        if own {
            return Err("the project's own folder is not a worktree".into());
        }
        let inside = |cwd: &Path| place::resolved(cwd).starts_with(&here);
        if self
            .sessions
            .iter()
            .any(|s| s.info.status.is_live() && inside(&s.info.cwd))
        {
            return Err("stop its cards first".into());
        }
        let (tx, project) = (self.removed_tx.clone(), w.project);
        tokio::task::spawn_blocking(move || {
            let result =
                crate::worktrees::remove(&view.path, &path, force, &crate::github::worktree::git);
            let _ = tx.send(Removed {
                client,
                project,
                path,
                result,
            });
        });
        Ok(())
    }

    /// The worktree is gone: forgotten, its stopped cards archived; or why not.
    pub fn worktree_removed(&mut self, r: Removed) {
        match r.result {
            Ok(Some(files)) => self.send(
                r.client,
                ServerEvent::RemoveRefused {
                    path: r.path,
                    files,
                },
            ),
            Ok(None) => {
                if let Err(e) = self.store.delete_worktree(&r.path) {
                    tracing::warn!(error = %e, "could not forget a worktree");
                }
                self.worktree_stats.remove(&r.path);
                // Its cards that stopped: their records stay, out of the grid.
                let inside = |cwd: &Path| cwd.starts_with(&r.path);
                let cards: Vec<SessionId> = self
                    .sessions
                    .iter()
                    .filter(|s| !s.info.archived && !s.info.status.is_live() && inside(&s.info.cwd))
                    .map(|s| s.info.id)
                    .collect();
                for id in cards {
                    if let Some(s) = self.session_mut(id) {
                        s.info.archived = true;
                    }
                    self.updated(id);
                }
                self.send(r.client, ServerEvent::WorktreeRemoved { path: r.path });
                self.send_worktrees(r.project);
            }
            Err(why) => {
                let name = r
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.send(
                    r.client,
                    ServerEvent::RemoveFailed {
                        path: r.path,
                        message: format!("couldn't remove {name} · {why}"),
                    },
                );
            }
        }
    }

    /// The project's worktrees as kept, with what each changed.
    fn worktree_infos(&self, project: ProjectId) -> Vec<termist_core::WorktreeInfo> {
        self.store
            .worktrees()
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.project == project)
            .map(|w| termist_core::WorktreeInfo {
                pr_end: w.repo.zip(w.pr).and_then(|(repo, number)| {
                    let pr = termist_core::github::PrRef { repo, number };
                    Some((number, self.github.pr_end(pr)?))
                }),
                stat: self.worktree_stats.get(&w.path).copied(),
                path: w.path,
                repo: w.repo,
                branch: w.branch,
                base: w.base,
                made_by_termist: w.made_by_termist,
                shown: w.shown,
            })
            .collect()
    }

    /// Sends the project's worktrees to every client when they changed.
    fn send_worktrees(&mut self, project: ProjectId) {
        let list = self.worktree_infos(project);
        if self.worktrees_sent.get(&project) == Some(&list) {
            return;
        }
        self.worktrees_sent.insert(project, list.clone());
        self.broadcast(ServerEvent::Worktrees { project, list });
    }

    /// Each session's place from what git said and the pull requests last read (those
    /// in `only` alone); a change is sent to every client.
    fn refresh_places(&mut self, only: Option<&Path>) {
        let mut views: HashMap<ProjectId, Vec<place::RepoView>> = HashMap::new();
        let mut changed = vec![];
        for s in &mut self.sessions {
            if only.is_some_and(|cwd| s.info.cwd != cwd) {
                continue;
            }
            let Some(facts) = self.git_facts.get(&s.info.cwd) else {
                continue; // not read yet
            };
            let repos = views
                .entry(s.info.project)
                .or_insert_with(|| self.github.repo_views(s.info.project));
            let new = Some(Box::new(place::place(&s.info.cwd, facts.as_ref(), repos)));
            if s.info.place != new {
                s.info.place = new;
                changed.push(s.info.clone());
            }
        }
        for info in changed {
            self.note_worktree_pr(&info);
            self.broadcast(ServerEvent::SessionUpdated(info));
        }
    }

    /// A card on a kept worktree's branch names its pull request: kept, so the band can
    /// say it merged once it is no longer open.
    fn note_worktree_pr(&mut self, info: &SessionInfo) {
        let Some((root, number)) = info
            .place
            .as_ref()
            .and_then(|p| Some((p.root.clone(), p.pr?.number)))
        else {
            return;
        };
        let kept = self.store.worktrees().unwrap_or_default();
        let Some(w) = kept
            .iter()
            .find(|w| place::resolved(&w.path) == place::resolved(&root))
        else {
            return;
        };
        if w.pr != Some(number) {
            if let Err(e) = self.store.set_worktree_pr(&w.path, number) {
                tracing::warn!(error = %e, "could not keep the worktree's pull request");
            }
            self.send_worktrees(w.project);
        }
    }

    /// The agent says it works in another folder now (Claude's hooks carry it).
    fn moved(&mut self, id: SessionId, cwd: PathBuf) {
        let Some(s) = self.session_mut(id) else {
            return;
        };
        if s.info.cwd == cwd || !cwd.is_absolute() {
            return;
        }
        s.info.cwd = cwd.clone();
        self.persist(id);
        match self.git_facts.contains_key(&cwd) {
            true => self.refresh_places(Some(&cwd)),
            false => self.read_place(cwd),
        }
    }

    /// Write-through: mirrors a session's current info into the store.
    fn persist(&self, id: SessionId) {
        if let Some(s) = self.session(id)
            && let Err(e) = self.store.upsert_session(&s.info, s.resumable)
        {
            tracing::warn!(session = %id, error = %e, "could not store session");
        }
    }

    fn state(&self) -> StateSnapshot {
        StateSnapshot {
            projects: self.projects.clone(),
            sessions: self.sessions.iter().map(|s| s.info.clone()).collect(),
            last_launch: self.last_launch.clone(),
        }
    }

    fn send(&self, client: ClientId, event: ServerEvent) {
        if let Some(out) = self.clients.get(&client) {
            let _ = out.send(event);
        }
    }

    fn broadcast(&self, event: ServerEvent) {
        for out in self.clients.values() {
            let _ = out.send(event.clone());
        }
    }

    /// Tells `client` why its request failed, if it did.
    fn report(&self, client: ClientId, result: anyhow::Result<()>) {
        if let Err(e) = result {
            self.send(
                client,
                ServerEvent::Error {
                    message: format!("{e:#}"),
                },
            );
        }
    }

    /// Stores and broadcasts a session's changed info.
    fn updated(&self, id: SessionId) {
        if let Some(s) = self.session(id) {
            self.persist(id);
            self.broadcast(ServerEvent::SessionUpdated(s.info.clone()));
        }
    }

    fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.info.id == id)
    }

    fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.info.id == id)
    }

    /// Applies a status signal; broadcasts only when the status actually changed.
    fn signal(&mut self, id: SessionId, signal: Signal) {
        let Some(s) = self.session_mut(id) else {
            return;
        };
        // A turn that goes on after a title cancel was never cancelled.
        let from = match (s.info.status, signal) {
            (AgentStatus::Finished, Signal::ToolDone | Signal::TurnStopped)
                if s.title_cancelled =>
            {
                AgentStatus::Running
            }
            (status, _) => status,
        };
        let next = from.apply(signal);
        if next == s.info.status {
            return;
        }
        s.title_cancelled = false;
        s.info.status = next;
        s.info.last_activity_ms = now_ms();
        let info = s.info.clone();
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Connected { client, out } => {
                self.clients.insert(client, out);
            }
            Msg::Disconnected(client) => {
                self.clients.remove(&client);
                self.github.gone(client);
                for s in &self.sessions {
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Detach { client });
                    }
                }
            }
            Msg::Request { client, req } => self.request(client, req),
        }
    }

    /// Runs GitHub's jobs on blocking threads and sends its events.
    fn github_effects(&mut self, fx: Effects) {
        for job in fx.jobs {
            let (tx, locate) = (self.github_tx.clone(), self.locate.clone());
            // A job that panics still reports, or its beat would stay in flight.
            let fallback = github::jobs::failed(&job);
            tokio::task::spawn_blocking(move || {
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    github::jobs::run(job, &locate)
                }))
                .unwrap_or_else(|_| {
                    tracing::error!("a GitHub job panicked");
                    fallback
                });
                let _ = tx.send(done);
            });
        }
        for (to, event) in fx.events {
            match to {
                To::All => self.broadcast(event),
                To::One(client) => self.send(client, event),
            }
        }
    }

    pub fn github_tick(&mut self) {
        let fx = self.github.tick(std::time::Instant::now(), &self.projects);
        self.github_effects(fx);
    }

    /// A GitHub job finished; what it found may start the next step at once.
    pub fn github_done(&mut self, done: Done) {
        let fx = self
            .github
            .done(done, std::time::Instant::now(), &self.store, &self.projects);
        self.github_effects(fx);
        self.github_tick();
        // A new list of pull requests may name a session's branch, or end one.
        self.refresh_places(None);
        let open: Vec<ProjectId> = self
            .projects
            .iter()
            .filter(|p| p.open)
            .map(|p| p.id)
            .collect();
        for project in open {
            self.send_worktrees(project);
        }
    }

    pub fn note(&mut self, note: SessionNote) {
        match note {
            SessionNote::Title(id, title) => {
                // Some agents animate their title; only a real change is stored and sent.
                if let Some(s) = self.session_mut(id)
                    && s.info.title != title
                {
                    let idle = s.info.kind
                        == (SessionKind::Agent {
                            harness: Harness::Claude,
                        })
                        && s.info.status == AgentStatus::Running
                        && title.as_deref().is_some_and(claude::title_is_idle);
                    s.idle_title_since = if idle {
                        s.idle_title_since
                            .or_else(|| Some(std::time::Instant::now()))
                    } else {
                        None
                    };
                    // A working title after a title cancel: the turn is still going.
                    let resumed = s.title_cancelled
                        && s.info.status == AgentStatus::Finished
                        && title
                            .as_deref()
                            .is_some_and(|t| !t.is_empty() && !claude::title_is_idle(t));
                    s.info.title = title;
                    let info = s.info.clone();
                    self.persist(id);
                    self.broadcast(ServerEvent::SessionUpdated(info));
                    if resumed {
                        self.signal(id, Signal::ToolDone);
                    }
                }
            }
            SessionNote::Activity(id) => {
                let due = self.session_mut(id).and_then(|s| {
                    s.info.last_activity_ms = now_ms();
                    let due = s
                        .activity_broadcast
                        .is_none_or(|t| t.elapsed() >= ACTIVITY_BROADCAST);
                    if due {
                        s.activity_broadcast = Some(std::time::Instant::now());
                    }
                    due.then(|| s.info.clone())
                });
                if let Some(info) = due {
                    self.broadcast(ServerEvent::SessionUpdated(info));
                }
            }
            SessionNote::Exited(id, code) => {
                // The idle title was the exited process's; it says nothing about the next.
                if let Some(s) = self.session_mut(id) {
                    s.idle_title_since = None;
                    s.title_cancelled = false;
                }
                self.signal(id, Signal::ProcessExited { code })
            }
        }
    }

    fn request(&mut self, client: ClientId, req: ClientRequest) {
        match req {
            ClientRequest::Hello { .. } => self.send(
                client,
                ServerEvent::Error {
                    message: "duplicate hello".into(),
                },
            ),
            ClientRequest::ListState => {
                self.send(client, ServerEvent::State(self.state()));
                self.send(client, ServerEvent::Harnesses(self.harnesses.clone()));
                if let Some(why) = self.store.not_saved() {
                    self.send(
                        client,
                        ServerEvent::Error {
                            message: why.to_string(),
                        },
                    );
                }
                let fx = self.github.snapshot(To::One(client), &self.projects);
                self.github_effects(fx);
                // A new client knows of no worktree: only projects that have some.
                for p in &self.projects {
                    let list = self.worktree_infos(p.id);
                    if list.is_empty() {
                        continue;
                    }
                    self.send(
                        client,
                        ServerEvent::Worktrees {
                            project: p.id,
                            list,
                        },
                    );
                }
            }
            ClientRequest::AddProject { path } => match self.add_project(path) {
                Ok(()) => self.broadcast(ServerEvent::State(self.state())),
                Err(e) => self.send(
                    client,
                    ServerEvent::Error {
                        message: format!("{e:#}"),
                    },
                ),
            },
            ClientRequest::CreateSession {
                project,
                kind,
                cwd,
                prompt,
                model,
                effort,
                cols,
                rows,
            } => {
                if let Err(e) =
                    self.create_session(project, kind, cwd, prompt, model, effort, (cols, rows))
                {
                    self.send(
                        client,
                        ServerEvent::Error {
                            message: format!("{e:#}"),
                        },
                    );
                }
            }
            ClientRequest::Attach {
                session,
                cols,
                rows,
            } => {
                if let (Some(s), Some(out)) = (self.session(session), self.clients.get(&client))
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Resize { cols, rows });
                    let _ = cmd.send(SessionCmd::Attach {
                        client,
                        out: out.clone(),
                    });
                }
            }
            ClientRequest::Detach { session } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Detach { client });
                }
            }
            ClientRequest::Input { session, data } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Input(data));
                }
                self.signal(session, Signal::UserTyped);
            }
            ClientRequest::Resize {
                session,
                cols,
                rows,
            } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Resize { cols, rows });
                }
            }
            ClientRequest::Scroll { session, scroll } => {
                if let Some(s) = self.session(session)
                    && let Some(cmd) = &s.cmd
                {
                    let _ = cmd.send(SessionCmd::Scroll(scroll));
                }
            }
            ClientRequest::MarkSeen { session } => self.signal(session, Signal::Seen),
            ClientRequest::KillSession { session } => {
                if let Some(pos) = self.sessions.iter().position(|s| s.info.id == session) {
                    let s = self.sessions.remove(pos);
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Kill);
                    }
                    if let Err(e) = self.store.delete_session(session) {
                        tracing::warn!(session = %session, error = %e, "could not delete session");
                    }
                    self.broadcast(ServerEvent::SessionRemoved(session));
                }
            }
            ClientRequest::Hook {
                session,
                harness,
                event,
                payload_json,
            } => {
                let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
                self.hook(session, harness, &event, &payload);
                self.send(client, ServerEvent::Ack);
            }
            ClientRequest::Resume {
                session,
                cols,
                rows,
            } => {
                if let Err(e) = self.resume(session, cols, rows) {
                    self.send(
                        client,
                        ServerEvent::Error {
                            message: format!("{e:#}"),
                        },
                    );
                }
            }
            ClientRequest::RenameSession { session, name } => {
                let result = self.rename(session, &name);
                self.report(client, result);
            }
            ClientRequest::ArchiveSession { session } => {
                let result = self.set_archived(session, true);
                self.report(client, result);
            }
            ClientRequest::UnarchiveSession { session } => {
                let result = self.set_archived(session, false);
                self.report(client, result);
            }
            ClientRequest::CloseProject { project } => {
                self.set_project_open(client, project, false)
            }
            ClientRequest::OpenProject { project } => self.set_project_open(client, project, true),
            ClientRequest::ListPromptHistory { limit } => {
                let limit = (limit as usize).min(PROMPT_HISTORY_MAX);
                let history = self.store.prompt_history(limit).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "could not read the prompt history");
                    vec![]
                });
                self.send(client, ServerEvent::PromptHistory(history));
            }
            ClientRequest::SetLastLaunch(launch) => {
                if let Err(e) = self.store.set_last_launch(&launch) {
                    tracing::warn!(error = %e, "could not store the last launch");
                }
                self.last_launch = Some(launch);
            }
            ClientRequest::ListModels { harness } => {
                self.list_models(client, harness, std::time::Instant::now());
            }
            ClientRequest::RescanHarnesses => self.rescan(std::time::Instant::now()),
            ClientRequest::SetColors(colors) => {
                self.colors = colors;
                for cmd in self.sessions.iter().filter_map(|s| s.cmd.as_ref()) {
                    let _ = cmd.send(SessionCmd::SetColors(colors));
                }
            }
            req @ (ClientRequest::SetGitHub { .. }
            | ClientRequest::SetPrFocus { .. }
            | ClientRequest::ListRepos { .. }
            | ClientRequest::SetRepoVisible { .. }
            | ClientRequest::SetRepoAccount { .. }
            | ClientRequest::RefreshPrs { .. }
            | ClientRequest::MarkPrSeen { .. }
            | ClientRequest::SetFileViewed { .. }
            | ClientRequest::WritePr { .. }
            | ClientRequest::OpenWorktree { .. }) => {
                let fx = self.github.request(
                    client,
                    req,
                    &self.store,
                    &self.projects,
                    std::time::Instant::now(),
                );
                self.github_effects(fx);
                self.github_tick();
            }
            // Worktrees of their own come with the next steps of this change.
            ClientRequest::CreateWorktree {
                project,
                repo,
                branch,
                ticket,
            } => self.create_worktree(client, project, repo, branch, ticket),
            ClientRequest::SetWorktreeShown { path, shown } => {
                let kept = self.store.worktrees().unwrap_or_default();
                if let Some(w) = kept.iter().find(|w| w.path == path) {
                    if let Err(e) = self.store.set_worktree_shown(&path, shown) {
                        tracing::warn!(error = %e, "could not keep the worktree's choice");
                    }
                    self.send_worktrees(w.project);
                }
            }
            ClientRequest::RemoveWorktree { path, force } => {
                if let Err(why) = self.remove_worktree(client, path.clone(), force) {
                    self.send(client, ServerEvent::RemoveFailed { path, message: why });
                }
            }
            ClientRequest::Shutdown => {
                for s in &self.sessions {
                    if let Some(cmd) = &s.cmd {
                        let _ = cmd.send(SessionCmd::Kill);
                    }
                }
                self.send(client, ServerEvent::Ack);
                if let Some(tx) = self.shutdown.take() {
                    let _ = tx.send(());
                }
            }
        }
    }

    fn hook(&mut self, id: SessionId, harness: Harness, event: &str, payload: &Value) {
        // Any hook means Claude is still talking to us: the title alone decides nothing.
        if let Some(s) = self.session_mut(id) {
            s.idle_title_since = None;
        }
        tracing::debug!(
            session = %id,
            harness = harness.id(),
            event,
            session_id = payload.get("session_id").and_then(serde_json::Value::as_str),
            source = payload.get("source").and_then(serde_json::Value::as_str),
            transcript_path = payload.get("transcript_path").and_then(serde_json::Value::as_str),
            hook_event_name = payload
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str),
            turn_id = payload.get("turn_id").and_then(serde_json::Value::as_str),
            opencode_session_id = payload
                .pointer("/properties/sessionID")
                .and_then(serde_json::Value::as_str),
            opencode_info_id = payload
                .pointer("/properties/info/id")
                .and_then(serde_json::Value::as_str),
            opencode_info_parent_id = payload
                .pointer("/properties/info/parentID")
                .and_then(serde_json::Value::as_str),
            "received hook"
        );
        let signal = match harness {
            Harness::Claude => {
                // `/clear` starts a new conversation, which exists only with its first prompt.
                let cleared = event == "SessionStart"
                    && payload.get("source").and_then(Value::as_str) == Some("clear")
                    && payload.get("session_id").and_then(Value::as_str)
                        != self
                            .session(id)
                            .and_then(|s| s.info.agent_session_id.as_deref());
                // Claude's id is known before its conversation exists: not a sign of one.
                self.capture_session_start(id, event, payload);
                if cleared {
                    self.forget_conversation(id);
                }
                if let Some(path) = payload.get("transcript_path").and_then(Value::as_str) {
                    self.watch_transcript(id, Path::new(path));
                }
                if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                    self.moved(id, PathBuf::from(cwd));
                }
                if event == "UserPromptSubmit" {
                    self.skip_transcript_so_far(id);
                }
                claude::signal_for(event, payload)
            }
            Harness::Codex if !self.codex_event_is_ours(id, payload) => {
                tracing::debug!(
                    session = %id,
                    event,
                    "ignored a hook from an unsaved Codex session"
                );
                None
            }
            Harness::Codex => {
                if self.capture_codex_session_start(id, event, payload) {
                    self.mark_resumable(id);
                }
                codex::signal_for(event, payload)
            }
            Harness::OpenCode => self.opencode_hook(id, event, payload),
        };
        if signal == Some(Signal::PromptSubmitted) {
            self.mark_resumable(id);
        }
        if let Some(signal) = signal {
            self.signal(id, signal);
        }
    }

    /// Takes the agent's id from a `SessionStart` payload; true when there was one.
    fn capture_session_start(&mut self, id: SessionId, event: &str, payload: &Value) -> bool {
        match payload.get("session_id").and_then(Value::as_str) {
            Some(sid) if event == "SessionStart" => {
                self.set_agent_session_id(id, sid);
                true
            }
            _ => false,
        }
    }

    /// Codex only: like `capture_session_start`, but a `SessionStart` for a different
    /// id that arrives mid-turn (Running or NeedsFeedback) is ignored. Codex's
    /// internal/sub-agent sessions inherit our `-c` hook flags and fire their own
    /// SessionStart while the user's turn is still running; a genuine switch to a new
    /// Codex conversation can only happen between turns, so this can't mistake one
    /// for the other. The SessionStart itself carries no status signal for Codex.
    /// Codex runs sessions of its own after a turn (they inherit the hook flags) that it
    /// never saves: their hooks carry no `transcript_path`. Only the card's own session, the
    /// first one it sees, or a saved session the user switched to may move the card.
    fn codex_event_is_ours(&self, id: SessionId, payload: &Value) -> bool {
        let Some(sid) = payload.get("session_id").and_then(Value::as_str) else {
            return true;
        };
        match self
            .session(id)
            .and_then(|s| s.info.agent_session_id.as_deref())
        {
            None => true,
            Some(known) if known == sid => true,
            Some(_) => payload
                .get("transcript_path")
                .and_then(Value::as_str)
                .is_some_and(|p| !p.is_empty()),
        }
    }

    fn capture_codex_session_start(&mut self, id: SessionId, event: &str, payload: &Value) -> bool {
        let Some(sid) = payload.get("session_id").and_then(Value::as_str) else {
            return false;
        };
        if event != "SessionStart" {
            return false;
        }
        if let Some(s) = self.session(id)
            && matches!(
                s.info.status,
                AgentStatus::Running | AgentStatus::NeedsFeedback
            )
            && s.info.agent_session_id.as_deref() != Some(sid)
        {
            tracing::debug!(
                session = %id,
                session_id = sid,
                "ignored mid-turn Codex SessionStart for another session"
            );
            return false;
        }
        self.set_agent_session_id(id, sid);
        true
    }

    /// The card follows the OpenCode session the user is in: a session created
    /// without a parent (`/new`) or a message in another session (switching) moves
    /// it there. Subagent sessions (created with a parent) never move the card.
    fn opencode_hook(&mut self, id: SessionId, event: &str, payload: &Value) -> Option<Signal> {
        let sid = opencode::event_session(payload).map(str::to_string);
        let s = self.session_mut(id)?;
        if event == "session.created" {
            match sid {
                Some(sid) if opencode::is_child_session(payload) => {
                    s.opencode_children.insert(sid);
                }
                Some(sid) => self.adopt_agent_session(id, &sid),
                None => {}
            }
            return None;
        }
        if let Some(sid) = sid {
            if s.opencode_children.contains(&sid) {
                return None;
            }
            if event == "chat.message" {
                self.adopt_agent_session(id, &sid);
            }
        }
        opencode::signal_for(event, payload)
    }

    /// An id the agent reported for a conversation it has: resume can use it.
    fn adopt_agent_session(&mut self, id: SessionId, sid: &str) {
        self.set_agent_session_id(id, sid);
        self.mark_resumable(id);
    }

    fn mark_resumable(&mut self, id: SessionId) {
        if let Some(s) = self.session_mut(id)
            && !s.resumable
        {
            s.resumable = true;
            self.persist(id);
        }
    }

    /// Resume starts the agent fresh until the conversation exists again.
    fn forget_conversation(&mut self, id: SessionId) {
        if let Some(s) = self.session_mut(id)
            && s.resumable
        {
            s.resumable = false;
            self.persist(id);
        }
    }

    fn set_agent_session_id(&mut self, id: SessionId, sid: &str) {
        if let Some(s) = self.session_mut(id)
            && s.info.agent_session_id.as_deref() != Some(sid)
        {
            s.info.agent_session_id = Some(sid.to_string());
            let info = s.info.clone();
            self.persist(id);
            self.broadcast(ServerEvent::SessionUpdated(info));
        }
    }

    fn watch_transcript(&mut self, id: SessionId, path: &Path) {
        if let Some(s) = self.session_mut(id)
            && s.transcript.as_ref().is_none_or(|t| t.path() != path)
        {
            s.transcript = Some(TranscriptTail::new(path.to_path_buf()));
        }
    }

    /// A new turn starts: an interrupt line already in the transcript (the previous
    /// turn's, not read yet because polling only runs mid-turn) must not cancel it.
    fn skip_transcript_so_far(&mut self, id: SessionId) {
        if let Some(t) = self.session_mut(id).and_then(|s| s.transcript.as_mut()) {
            t.poll();
        }
    }

    /// Claude cards still running with an idle title and no hook for `IDLE_TITLE_CANCEL`:
    /// the turn was cancelled before its answer started.
    pub fn poll_idle_titles(&mut self, now: std::time::Instant) {
        let cancelled: Vec<SessionId> = self
            .sessions
            .iter_mut()
            .filter(|s| s.info.status == AgentStatus::Running)
            .filter_map(|s| {
                let since = s.idle_title_since?;
                (now.duration_since(since) >= IDLE_TITLE_CANCEL).then(|| {
                    s.idle_title_since = None;
                    s.info.id
                })
            })
            .collect();
        for id in cancelled {
            self.signal(id, Signal::Cancelled);
            if let Some(s) = self.session_mut(id) {
                s.title_cancelled = true;
            }
        }
    }

    /// Claude sessions that are mid-turn and whose transcript shows an interrupt.
    pub fn poll_transcripts(&mut self) {
        let mut cancelled = Vec::new();
        for s in &mut self.sessions {
            if matches!(
                s.info.status,
                AgentStatus::Running | AgentStatus::NeedsFeedback
            ) && let Some(t) = s.transcript.as_mut()
                && t.poll()
            {
                cancelled.push(s.info.id);
            }
        }
        for id in cancelled {
            self.signal(id, Signal::Cancelled);
        }
    }

    /// A known path reopens its project; an unknown one is added.
    fn add_project(&mut self, path: PathBuf) -> anyhow::Result<()> {
        let path = std::fs::canonicalize(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        if let Some(p) = self.projects.iter_mut().find(|p| p.path == path) {
            if !p.open {
                p.open = true;
                if let Err(e) = self.store.upsert_project(p) {
                    tracing::warn!(error = %e, "could not store project");
                }
            }
            return Ok(());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.projects.push(ProjectInfo {
            id: ProjectId::new(),
            name,
            path,
            open: true,
        });
        if let Err(e) = self.store.upsert_project(self.projects.last().unwrap()) {
            tracing::warn!(error = %e, "could not store project");
        }
        Ok(())
    }

    /// Looks again, on a blocking thread, for the CLIs that were missing (one rescan at
    /// a time, and none within `RESCAN_INTERVAL` of the last one's start). OpenCode also
    /// needs its plugin, so it is written again when found.
    fn rescan(&mut self, now: std::time::Instant) {
        let missing: Vec<Harness> = self
            .harnesses
            .iter()
            .filter(|h| !h.available)
            .map(|h| h.harness)
            .collect();
        let too_soon = self
            .last_rescan
            .is_some_and(|t| now.saturating_duration_since(t) < RESCAN_INTERVAL);
        if self.rescanning || too_soon || missing.is_empty() {
            return;
        }
        self.rescanning = true;
        self.last_rescan = Some(now);
        let (tx, find) = (self.rescans.clone(), self.find_program);
        let opencode_dir = self.launcher.opencode_config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let found = missing
                .into_iter()
                .filter_map(|harness| {
                    let program = find(harness.program())?;
                    if harness == Harness::OpenCode
                        && let Err(e) = opencode::write_plugin(&opencode_dir)
                    {
                        tracing::warn!(error = %e, "could not write the OpenCode plugin");
                        return None;
                    }
                    Some((harness, program))
                })
                .collect();
            let _ = tx.send(Rescanned(found));
        });
    }

    /// Answers at once with the recent models and the catalog as it is; a catalog older
    /// than `CATALOG_FRESH`, an empty one, or none is read again, and sent when it comes.
    pub fn list_models(&mut self, client: ClientId, harness: Harness, now: std::time::Instant) {
        let recent = self.store.recent_models(harness);
        if harness == Harness::Claude {
            let catalog = crate::models::claude();
            self.send(
                client,
                ServerEvent::Models {
                    harness,
                    recent,
                    catalog,
                },
            );
            return;
        }
        let (catalog, fresh) = match self.catalogs.get(&harness) {
            Some((list, at)) => (
                list.clone(),
                !list.is_empty() && now.saturating_duration_since(*at) < CATALOG_FRESH,
            ),
            None => (vec![], false),
        };
        self.send(
            client,
            ServerEvent::Models {
                harness,
                recent,
                catalog,
            },
        );
        if fresh {
            return;
        }
        let waiters = self.catalog_waiters.entry(harness).or_default();
        let reading = !waiters.is_empty();
        if !waiters.contains(&client) {
            waiters.push(client);
        }
        if reading {
            return;
        }
        let (tx, read) = (self.catalog_tx.clone(), self.read_catalog);
        let program = self.launcher.programs.get(harness).to_string();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Catalog(harness, read(harness, &program)));
        });
    }

    /// A catalog was read: it is kept, and sent to every client that waited for it.
    pub fn catalog_read(&mut self, Catalog(harness, list): Catalog, now: std::time::Instant) {
        if list.is_empty() {
            tracing::info!(?harness, "the CLI listed no models");
        }
        self.catalogs.insert(harness, (list.clone(), now));
        let recent = self.store.recent_models(harness);
        for client in self.catalog_waiters.remove(&harness).unwrap_or_default() {
            self.send(
                client,
                ServerEvent::Models {
                    harness,
                    recent: recent.clone(),
                    catalog: list.clone(),
                },
            );
        }
    }

    /// A rescan finished: newly found CLIs become available to every client, and their
    /// cached catalogs (read while they were missing) are dropped.
    pub fn rescanned(&mut self, Rescanned(found): Rescanned) {
        self.rescanning = false;
        if found.is_empty() {
            return;
        }
        for (harness, program) in found {
            self.catalogs.remove(&harness);
            self.launcher
                .programs
                .set(harness, program.display().to_string());
            if let Some(h) = self.harnesses.iter_mut().find(|h| h.harness == harness) {
                h.available = true;
            }
        }
        tracing::info!(harnesses = ?self.harnesses, "agent CLIs after a rescan");
        self.broadcast(ServerEvent::Harnesses(self.harnesses.clone()));
    }

    /// Closing hides the tab; the sessions keep running. Every client gets the new state.
    fn set_project_open(&mut self, client: ClientId, project: ProjectId, open: bool) {
        let Some(p) = self.projects.iter_mut().find(|p| p.id == project) else {
            self.report(client, Err(anyhow::anyhow!("unknown project")));
            return;
        };
        p.open = open;
        if let Err(e) = self.store.upsert_project(p) {
            tracing::warn!(error = %e, "could not store project");
        }
        self.broadcast(ServerEvent::State(self.state()));
    }

    /// The new name sticks: terminal titles no longer replace it.
    fn rename(&mut self, id: SessionId, name: &str) -> anyhow::Result<()> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            bail!("a session name cannot be empty");
        }
        let Some(s) = self.session_mut(id) else {
            bail!("unknown session")
        };
        s.info.name = name;
        s.info.user_named = true;
        self.updated(id);
        Ok(())
    }

    /// Archiving stops a live session; the record (and the agent's conversation) stays.
    fn set_archived(&mut self, id: SessionId, archived: bool) -> anyhow::Result<()> {
        let Some(s) = self.session_mut(id) else {
            bail!("unknown session")
        };
        if archived
            && s.info.status.is_live()
            && let Some(cmd) = &s.cmd
        {
            let _ = cmd.send(SessionCmd::Kill);
        }
        s.info.archived = archived;
        self.updated(id);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn create_session(
        &mut self,
        project: ProjectId,
        kind: SessionKind,
        cwd: Option<PathBuf>,
        prompt: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        (cols, rows): (u16, u16),
    ) -> anyhow::Result<()> {
        let Some(root) = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.path.clone())
        else {
            bail!("unknown project")
        };
        // A shell has no model or effort; an agent only takes an effort its CLI knows,
        // or one the chosen model lists in the CLI's catalog.
        let (model, effort) = match &kind {
            SessionKind::Shell => (None, None),
            SessionKind::Agent { harness } => {
                let model = model
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty());
                if let Some(e) = &effort
                    && !harness.efforts().contains(&e.as_str())
                    && !self.model_has_effort(*harness, model.as_deref(), e)
                    && !self.effort_before_catalog(*harness, e)
                {
                    bail!("{} has no effort level {e:?}", harness.id());
                }
                (model, effort)
            }
        };
        let cwd = match cwd {
            None => root,
            Some(dir) => self.folder_of(project, &root, dir)?,
        };
        let id = SessionId::new();
        let launch = self.launcher.launch(LaunchRequest {
            id,
            kind: &kind,
            prompt: prompt.as_deref(),
            model: model.as_deref(),
            effort: effort.as_deref(),
            cwd: &cwd,
            cols,
            rows,
            resume: None,
        });
        let program = launch.spec.program.clone();
        let cmd = session::spawn(launch.spec, self.colors, self.notes.clone())
            .with_context(|| format!("could not start {} ({program})", kind.label()))?;
        self.remember_launch(&kind, prompt.as_deref(), model.as_deref());
        self.created += 1;
        let info = SessionInfo {
            id,
            project,
            name: format!("{}-{}", kind.label(), self.created),
            kind,
            status: AgentStatus::Fresh,
            agent_session_id: launch.agent_session_id,
            title: None,
            last_activity_ms: now_ms(),
            model,
            effort,
            user_named: false,
            archived: false,
            cwd: cwd.clone(),
            place: None,
        };
        self.sessions
            .push(Session::new(info.clone(), Some(cmd), false));
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
        self.read_place(cwd);
        Ok(())
    }

    /// `dir` when a session of the project may run there: a folder in the project, or a
    /// worktree of one of its repos (a sibling folder, as termist opens them).
    fn folder_of(
        &mut self,
        project: ProjectId,
        root: &Path,
        dir: PathBuf,
    ) -> anyhow::Result<PathBuf> {
        if !dir.is_dir() {
            bail!("{} is not a folder", dir.display());
        }
        if place::resolved(&dir).starts_with(place::resolved(root)) {
            return Ok(dir);
        }
        let facts = place::read(&dir, &self.git);
        let ours = facts.as_ref().is_some_and(|f| {
            self.github
                .repo_views(project)
                .iter()
                .any(|r| r.main == f.main)
        });
        if !ours {
            bail!("{} is not a folder of this project", dir.display());
        }
        self.git_facts.insert(dir.clone(), facts);
        Ok(dir)
    }

    /// The model is in the cached catalog of its CLI and lists this effort.
    fn model_has_effort(&self, harness: Harness, model: Option<&str>, effort: &str) -> bool {
        let Some(model) = model else { return false };
        self.catalogs.get(&harness).is_some_and(|(list, _)| {
            list.iter()
                .any(|m| m.id == model && m.efforts.iter().any(|e| e == effort))
        })
    }

    /// The remembered launch after a daemon restart: the CLI's model list has not been
    /// read yet (it is read when Ctrl+O opens), so the model's own efforts are unknown.
    /// Any effort an agent CLI is known to take goes through until then.
    fn effort_before_catalog(&self, harness: Harness, effort: &str) -> bool {
        harness != Harness::Claude
            && !harness.efforts().is_empty()
            && !self.catalogs.contains_key(&harness)
            && termist_core::EFFORT_LEVELS.contains(&effort)
    }

    /// A started session's prompt goes into the history, its model into the recent models.
    fn remember_launch(&self, kind: &SessionKind, prompt: Option<&str>, model: Option<&str>) {
        if let Some(p) = prompt.filter(|p| !p.trim().is_empty())
            && let Err(e) = self.store.add_prompt(p)
        {
            tracing::warn!(error = %e, "could not store the prompt");
        }
        if let (SessionKind::Agent { harness }, Some(m)) = (kind, model)
            && let Err(e) = self.store.add_recent_model(*harness, m)
        {
            tracing::warn!(error = %e, "could not store the model");
        }
    }

    fn resume(&mut self, id: SessionId, cols: u16, rows: u16) -> anyhow::Result<()> {
        let Some(pos) = self.sessions.iter().position(|s| s.info.id == id) else {
            bail!("unknown session")
        };
        let info = &self.sessions[pos].info;
        if info.status.is_live() {
            bail!("{} is still running", info.name);
        }
        let Some(project) = self.projects.iter().find(|p| p.id == info.project) else {
            bail!("the project of {} is gone", info.name)
        };
        // Its own folder while it is there; a worktree may have been removed since.
        let cwd = if info.cwd.is_dir() {
            info.cwd.clone()
        } else {
            project.path.clone()
        };
        let kind = info.kind.clone();
        let (model, effort) = (info.model.clone(), info.effort.clone());
        // Without a conversation to resume (no prompt yet), start the agent fresh.
        let resume = if self.sessions[pos].resumable {
            info.agent_session_id.clone()
        } else {
            None
        };
        let launch = self.launcher.launch(LaunchRequest {
            id,
            kind: &kind,
            prompt: None,
            model: model.as_deref(),
            effort: effort.as_deref(),
            cwd: &cwd,
            cols,
            rows,
            resume: resume.as_deref(),
        });
        let program = launch.spec.program.clone();
        let cmd = session::spawn(launch.spec, self.colors, self.notes.clone())
            .with_context(|| format!("could not start {} ({program})", kind.label()))?;
        let s = &mut self.sessions[pos];
        s.cmd = Some(cmd); // dropping the old sender ends the old session task
        s.transcript = None;
        s.activity_broadcast = None;
        s.idle_title_since = None;
        s.title_cancelled = false;
        s.info.title = None; // the new process sets its own
        s.info.status = AgentStatus::Fresh;
        s.resumable = resume.is_some();
        s.info.agent_session_id = launch.agent_session_id;
        s.info.last_activity_ms = now_ms();
        s.info.cwd = cwd.clone();
        let info = s.info.clone();
        self.persist(id);
        self.broadcast(ServerEvent::SessionUpdated(info));
        self.read_place(cwd);
        Ok(())
    }
}

pub async fn run(
    mut reg: Registry,
    mut rx: UnboundedReceiver<Msg>,
    mut notes: UnboundedReceiver<SessionNote>,
) {
    let mut transcripts = tokio::time::interval(Duration::from_millis(500));
    transcripts.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut rescans = reg.rescans_rx.take().expect("a registry runs once");
    let mut catalogs = reg.catalog_rx.take().expect("a registry runs once");
    let mut github = reg.github_rx.take().expect("a registry runs once");
    let mut places = reg.places_rx.take().expect("a registry runs once");
    let mut scans = reg.scans_rx.take().expect("a registry runs once");
    let mut made = reg.made_rx.take().expect("a registry runs once");
    let mut removed = reg.removed_rx.take().expect("a registry runs once");
    let mut github_beat = tokio::time::interval(Duration::from_secs(1));
    github_beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(msg) => reg.handle(msg),
                None => break,
            },
            Some(note) = notes.recv() => reg.note(note),
            Some(found) = rescans.recv() => reg.rescanned(found),
            Some(c) = catalogs.recv() => reg.catalog_read(c, std::time::Instant::now()),
            Some(done) = github.recv() => reg.github_done(done),
            Some(read) = places.recv() => reg.place_read(read),
            Some(scan) = scans.recv() => reg.worktrees_scanned(scan),
            Some(m) = made.recv() => reg.worktree_made(m),
            Some(r) = removed.recv() => reg.worktree_removed(r),
            _ = github_beat.tick() => {
                reg.github_tick();
                reg.places_tick(std::time::Instant::now());
            }
            _ = transcripts.tick() => {
                reg.poll_transcripts();
                reg.poll_idle_titles(std::time::Instant::now());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{DaemonConfig, HarnessPrograms};
    use crate::store::Store;
    use tokio::sync::mpsc::unbounded_channel;

    fn registry_with(project: &ProjectInfo, sessions: &[SessionInfo]) -> Registry {
        let store = Store::open_in_memory();
        store.upsert_project(project).unwrap();
        for s in sessions {
            store.upsert_session(s, false).unwrap();
        }
        registry_on(store)
    }

    fn registry_on(store: Store) -> Registry {
        let launcher = Launcher {
            config: DaemonConfig::default(),
            programs: HarnessPrograms {
                claude: "claude".into(),
                codex: "codex".into(),
                opencode: "opencode".into(),
            },
            exe: PathBuf::from("/t/termist"),
            claude_settings: PathBuf::from("/t/claude-hooks.json"),
            runtime_dir: PathBuf::from("/t/run"),
            termist_home: None,
            opencode_config_dir: PathBuf::from("/t/opencode"),
        };
        let (notes, _notes_rx) = unbounded_channel();
        let (stop, _stop_rx) = oneshot::channel();
        Registry::new(launcher, vec![], store, notes, stop)
    }

    fn project() -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: std::env::temp_dir(),
            open: true,
        }
    }

    fn stored(p: &ProjectInfo, name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project: p.id,
            kind: SessionKind::Shell,
            name: name.into(),
            status: AgentStatus::Disconnected,
            agent_session_id: None,
            title: None,
            last_activity_ms: 1,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: p.path.clone(),
            place: None,
        }
    }

    fn connect(reg: &mut Registry) -> UnboundedReceiver<ServerEvent> {
        let (out, rx) = unbounded_channel();
        reg.handle(Msg::Connected {
            client: ClientId(1),
            out,
        });
        rx
    }

    // A client learns at once that nothing it does will be kept.
    #[test]
    fn every_client_is_told_when_sessions_are_not_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        let mut reg = registry_on(Store::open(&path).unwrap());
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::ListState,
        });
        assert!(matches!(rx.try_recv(), Ok(ServerEvent::State(_))));
        assert!(matches!(rx.try_recv(), Ok(ServerEvent::Harnesses(_))));
        match rx.try_recv() {
            Ok(ServerEvent::Error { message }) => assert!(
                message.starts_with("sessions are not being saved")
                    && message.contains("schema 99"),
                "{message}"
            ),
            other => panic!("{other:?}"),
        }

        let p = project();
        let mut reg = registry_with(&p, &[]);
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::ListState,
        });
        let _ = (rx.try_recv(), rx.try_recv());
        assert!(rx.try_recv().is_err(), "a saving store says nothing");
    }

    #[test]
    fn names_keep_counting_from_the_highest_stored_number() {
        let p = project();
        let names = [
            "claude-1", "claude-3", "shell-2", "my-notes", "codex-x", "odd",
        ];
        let sessions: Vec<_> = names.iter().map(|n| stored(&p, n)).collect();
        assert_eq!(registry_with(&p, &sessions).created, 3);
        assert_eq!(registry_with(&p, &[]).created, 0);
    }

    #[test]
    fn a_repeated_title_is_neither_stored_nor_broadcast_again() {
        let p = project();
        let s = stored(&p, "codex-1");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        let mut rx = connect(&mut reg);
        for _ in 0..2 {
            reg.note(SessionNote::Title(s.id, Some("Working".into())));
        }
        let mut updates = vec![];
        while let Ok(ev) = rx.try_recv() {
            updates.push(ev);
        }
        assert_eq!(updates.len(), 1, "{updates:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resume_starts_with_no_title_and_a_fresh_activity_window() {
        let p = project();
        let mut s = stored(&p, "shell-1");
        s.title = Some("old title".into());
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().activity_broadcast = Some(std::time::Instant::now());
        let _rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::Resume {
                session: s.id,
                cols: 80,
                rows: 24,
            },
        });
        let back = reg.session(s.id).unwrap();
        assert_eq!(back.info.status, AgentStatus::Fresh);
        assert_eq!(back.info.title, None);
        assert_eq!(back.activity_broadcast, None);
        assert_eq!(
            reg.store.load().unwrap().1[0].info.title,
            None,
            "stored too"
        );
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::KillSession { session: s.id },
        });
    }

    #[test]
    fn activity_is_broadcast_at_most_once_per_window() {
        let p = project();
        let s = stored(&p, "shell-1");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        let mut rx = connect(&mut reg);
        reg.note(SessionNote::Activity(s.id));
        reg.note(SessionNote::Activity(s.id));
        let mut updates = vec![];
        while let Ok(ev) = rx.try_recv() {
            updates.push(ev);
        }
        assert_eq!(updates.len(), 1, "{updates:?}");
        match &updates[0] {
            ServerEvent::SessionUpdated(info) => assert!(info.last_activity_ms > 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_effort_the_cli_does_not_know_is_refused_before_anything_starts() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let agent = |harness| SessionKind::Agent { harness };
        for (harness, effort) in [
            (Harness::Claude, "turbo"),
            (Harness::Claude, "ultra"),
            (Harness::OpenCode, "high"),
        ] {
            let err = reg
                .create_session(
                    p.id,
                    agent(harness),
                    None,
                    None,
                    None,
                    Some(effort.into()),
                    (80, 24),
                )
                .unwrap_err();
            assert!(err.to_string().contains("no effort level"), "{err}");
        }
        assert!(reg.sessions.is_empty());
    }

    fn gpt_x() -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "gpt-x".into(),
            label: "GPT-X".into(),
            efforts: vec!["low".into(), "xhigh".into()],
        }]
    }

    // The picker offers each Codex model's own efforts; the daemon takes them too.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_effort_of_the_chosen_model_in_the_catalog_is_accepted() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.launcher.programs.codex = "true".into();
        reg.catalogs
            .insert(Harness::Codex, (gpt_x(), std::time::Instant::now()));
        reg.create_session(
            p.id,
            SessionKind::Agent {
                harness: Harness::Codex,
            },
            None,
            None,
            Some("gpt-x".into()),
            Some("xhigh".into()),
            (80, 24),
        )
        .unwrap();
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.sessions[0].info.effort.as_deref(), Some("xhigh"));
        if let Some(cmd) = &reg.sessions[0].cmd {
            let _ = cmd.send(SessionCmd::Kill);
        }
    }

    // After a daemon restart the catalogs are empty until Ctrl+O; the remembered launch
    // (model and effort) must still start.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_known_effort_is_taken_before_the_catalog_is_read() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.launcher.programs.codex = "true".into();
        assert!(reg.catalogs.is_empty());
        reg.create_session(
            p.id,
            SessionKind::Agent {
                harness: Harness::Codex,
            },
            None,
            None,
            Some("gpt-x".into()),
            Some("xhigh".into()),
            (80, 24),
        )
        .unwrap();
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.sessions[0].info.effort.as_deref(), Some("xhigh"));
        if let Some(cmd) = &reg.sessions[0].cmd {
            let _ = cmd.send(SessionCmd::Kill);
        }
    }

    #[test]
    fn an_unknown_effort_is_refused_before_the_catalog_is_read() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let err = reg
            .create_session(
                p.id,
                SessionKind::Agent {
                    harness: Harness::Codex,
                },
                None,
                None,
                Some("gpt-x".into()),
                Some("turbo".into()),
                (80, 24),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no effort level"), "{err}");
        assert!(reg.sessions.is_empty());
    }

    #[test]
    fn a_model_effort_is_refused_for_a_model_not_in_the_catalog() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let codex = || SessionKind::Agent {
            harness: Harness::Codex,
        };
        let refused = |reg: &mut Registry, model: &str| {
            let err = reg
                .create_session(
                    p.id,
                    codex(),
                    None,
                    None,
                    Some(model.into()),
                    Some("xhigh".into()),
                    (80, 24),
                )
                .unwrap_err();
            assert!(err.to_string().contains("no effort level"), "{err}");
        };
        reg.catalogs
            .insert(Harness::Codex, (gpt_x(), std::time::Instant::now()));
        refused(&mut reg, "gpt-y");
        assert!(reg.sessions.is_empty());
    }

    // An archived card stays archived whatever its hooks say; only its status moves.
    #[test]
    fn hooks_move_an_archived_sessions_status_but_keep_it_archived() {
        let p = project();
        let mut s = stored(&p, "claude-1");
        s.kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        s.archived = true;
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().info.status = AgentStatus::Running;
        let mut rx = connect(&mut reg);
        reg.hook(s.id, Harness::Claude, "Stop", &Value::Null);
        match rx.try_recv() {
            Ok(ServerEvent::SessionUpdated(info)) => {
                assert_eq!(info.status, AgentStatus::Unseen);
                assert!(info.archived);
            }
            other => panic!("{other:?}"),
        }
        assert!(reg.store.load().unwrap().1[0].info.archived, "stored too");
    }

    fn claude(p: &ProjectInfo, agent_id: &str) -> SessionInfo {
        let mut s = stored(p, "claude-1");
        s.kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        s.agent_session_id = Some(agent_id.into());
        s
    }

    #[test]
    fn a_cleared_claude_conversation_is_not_resumable_until_its_first_prompt() {
        let p = project();
        let s = claude(&p, "old");
        let mut reg = registry_with(&p, std::slice::from_ref(&s));
        reg.session_mut(s.id).unwrap().resumable = true;
        let start =
            |sid: &str, source: &str| serde_json::json!({ "session_id": sid, "source": source });
        reg.hook(
            s.id,
            Harness::Claude,
            "SessionStart",
            &start("old", "resume"),
        );
        assert!(
            reg.session(s.id).unwrap().resumable,
            "a resumed conversation is kept"
        );
        reg.hook(
            s.id,
            Harness::Claude,
            "SessionStart",
            &start("new", "clear"),
        );
        let cleared = reg.session(s.id).unwrap();
        assert_eq!(cleared.info.agent_session_id.as_deref(), Some("new"));
        assert!(!cleared.resumable);
        assert!(!reg.store.load().unwrap().1[0].resumable, "stored too");
        reg.hook(s.id, Harness::Claude, "UserPromptSubmit", &Value::Null);
        assert!(reg.session(s.id).unwrap().resumable);
    }

    fn missing_codex_and_opencode(reg: &mut Registry) {
        reg.harnesses = Harness::ALL
            .into_iter()
            .map(|harness| HarnessInfo {
                harness,
                available: harness == Harness::Claude,
            })
            .collect();
    }

    #[tokio::test]
    async fn a_rescan_looks_only_for_the_missing_clis_and_tells_every_client() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let tmp = tempfile::tempdir().unwrap();
        reg.launcher.opencode_config_dir = tmp.path().join("opencode");
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |name| Some(PathBuf::from(format!("/new/{name}")));
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        let mut results = reg.rescans_rx.take().unwrap();
        let found = results.recv().await.unwrap();
        assert!(results.try_recv().is_err(), "one rescan at a time");
        reg.rescanned(found);
        match rx.try_recv() {
            Ok(ServerEvent::Harnesses(list)) => assert!(list.iter().all(|h| h.available)),
            other => panic!("{other:?}"),
        }
        assert_eq!(reg.launcher.programs.get(Harness::Codex), "/new/codex");
        assert_eq!(
            reg.launcher.programs.get(Harness::OpenCode),
            "/new/opencode"
        );
        assert_eq!(
            reg.launcher.programs.get(Harness::Claude),
            "claude",
            "an available CLI is not looked up again"
        );
        assert!(tmp.path().join("opencode/plugins/termist.ts").is_file());
    }

    #[tokio::test]
    async fn a_rescan_that_finds_nothing_changes_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |_| None;
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::RescanHarnesses,
        });
        let found = reg.rescans_rx.as_mut().unwrap().recv().await.unwrap();
        reg.rescanned(found);
        assert!(rx.try_recv().is_err(), "nothing to tell");
        assert!(!reg.rescanning, "the next rescan may run");
    }

    // Every `n` and `p` asks for a rescan; with a CLI missing, each would start a login
    // shell again. One rescan per window is enough.
    #[tokio::test]
    async fn a_rescan_runs_at_most_once_per_window() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        missing_codex_and_opencode(&mut reg);
        reg.find_program = |_| None;
        let mut results = reg.rescans_rx.take().unwrap();
        let t0 = std::time::Instant::now();
        reg.rescan(t0);
        reg.rescanned(results.recv().await.unwrap());
        reg.rescan(t0 + Duration::from_secs(10));
        assert!(!reg.rescanning, "too soon after the last one");
        tokio::task::yield_now().await;
        assert!(results.try_recv().is_err());
        reg.rescan(t0 + RESCAN_INTERVAL);
        assert!(reg.rescanning, "the window has passed");
        results.recv().await.unwrap();
    }

    fn running_claude(reg: &mut Registry, p: &ProjectInfo) -> SessionId {
        let s = claude(p, "c-1");
        reg.sessions.push(Session::new(s.clone(), None, true));
        reg.session_mut(s.id).unwrap().info.status = AgentStatus::Running;
        s.id
    }

    fn later(ms: u64) -> std::time::Instant {
        std::time::Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn an_idle_title_with_no_hook_for_a_while_cancels_a_running_claude_turn() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✶ Fix login".into())));
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.poll_idle_titles(later(1000));
        assert_eq!(
            reg.session(id).unwrap().info.status,
            AgentStatus::Running,
            "too early"
        );
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
    }

    #[test]
    fn a_hook_or_a_working_title_keeps_the_turn_running() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.hook(id, Harness::Claude, "PostToolUse", &Value::Null);
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
        reg.note(SessionNote::Title(id, Some("✳ Fix login again".into())));
        reg.note(SessionNote::Title(id, Some("✻ Fix login again".into())));
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
    }

    // The idle timer belongs to the process that showed the title: once it exits, a
    // resumed process starts without it.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_idle_timer_does_not_outlive_its_process() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.launcher
            .programs
            .set(Harness::Claude, "/usr/bin/true".into());
        let id = running_claude(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.note(SessionNote::Exited(id, Some(0)));
        assert_eq!(reg.session(id).unwrap().idle_title_since, None);
        reg.session_mut(id).unwrap().idle_title_since = Some(std::time::Instant::now());
        reg.resume(id, 80, 24).unwrap();
        // the new process's turn starts
        reg.session_mut(id).unwrap().info.status = AgentStatus::Running;
        reg.poll_idle_titles(later(5000));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_resumes_in_its_folder_or_its_project_s_when_the_folder_is_gone() {
        let p = project();
        let here = tempfile::tempdir().unwrap();
        let mut old = claude(&p, "a1");
        old.cwd = std::path::PathBuf::new(); // stored before sessions had folders
        let mut kept = claude(&p, "a2");
        kept.cwd = here.path().to_path_buf();
        let mut gone = claude(&p, "a3");
        gone.cwd = here.path().join("removed-worktree");
        let mut reg = registry_with(&p, &[old.clone(), kept.clone(), gone.clone()]);
        reg.launcher
            .programs
            .set(Harness::Claude, "/usr/bin/true".into());
        assert_eq!(
            reg.session(old.id).unwrap().info.cwd,
            p.path,
            "the project's"
        );
        reg.resume(kept.id, 80, 24).unwrap();
        assert_eq!(reg.session(kept.id).unwrap().info.cwd, here.path());
        reg.resume(gone.id, 80, 24).unwrap();
        assert_eq!(reg.session(gone.id).unwrap().info.cwd, p.path);
    }

    #[tokio::test]
    async fn a_claude_hook_moves_its_card_and_git_tells_its_branch_and_repo() {
        let p = project();
        let site = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let repo = store
            .upsert_repo(p.id, site.path(), "acme", "site")
            .unwrap();
        let card = claude(&p, "a1");
        store.upsert_session(&card, false).unwrap();
        let mut reg = registry_on(store);
        let mut rx = connect(&mut reg);
        // Absolute on every OS; git is not asked here, so it need not exist.
        let worktree = site.path().join("site-worktrees").join("fix-login");
        let payload = serde_json::json!({ "cwd": worktree, "session_id": "a1" });
        reg.hook(card.id, Harness::Claude, "PreToolUse", &payload);
        assert_eq!(reg.session(card.id).unwrap().info.cwd, worktree);
        assert!(reg.places_reading.contains(&worktree), "git is asked");
        assert_eq!(
            reg.store.load().unwrap().1[0].info.cwd,
            worktree,
            "kept for a restart"
        );
        reg.place_read(PlaceRead(
            worktree.clone(),
            Some(GitFacts {
                root: worktree.clone(),
                branch: Some("fix/login".into()),
                commit: None,
                main: place::resolved(site.path()),
            }),
        ));
        let place = reg.session(card.id).unwrap().info.place.clone().unwrap();
        assert_eq!(
            (place.branch.as_deref(), place.repo, place.pr),
            (Some("fix/login"), Some(repo.id), None)
        );
        let mut sent = vec![];
        while let Ok(ev) = rx.try_recv() {
            sent.push(ev);
        }
        assert!(sent.iter().any(|e| matches!(
            e,
            ServerEvent::SessionUpdated(u) if u.id == card.id && u.place.is_some()
        )));
        // The same answer again changes nothing and is not sent again.
        reg.place_read(PlaceRead(
            worktree.clone(),
            reg.git_facts[&worktree].clone(),
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_scan_keeps_new_worktrees_drops_gone_ones_and_says_so_once() {
        let p = project();
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let repo = store.upsert_repo(p.id, &p.path, "acme", "site").unwrap();
        let made = PathBuf::from("/w/site-worktrees/fix");
        store
            .upsert_worktree(&StoredWorktree {
                project: p.id,
                repo: Some(repo.id),
                path: made.clone(),
                branch: Some("fix".into()),
                base: Some("main".into()),
                pr: None,
                made_by_termist: true,
                shown: true,
            })
            .unwrap();
        let mut reg = registry_on(store);
        let mut rx = connect(&mut reg);
        let listed = |path: &PathBuf, branch: &str| Listed {
            path: path.clone(),
            branch: Some(branch.into()),
            main: false,
        };
        let outside = PathBuf::from("/w/site/.claude/worktrees/x");
        let stat = termist_core::Stat {
            files: 3,
            added: 60,
            removed: 28,
            dirty: true,
        };
        let scan = |found: Vec<(Listed, Option<termist_core::Stat>)>| WorktreeScan {
            project: p.id,
            repo: repo.id,
            found: Some(found),
        };
        reg.worktrees_scanned(scan(vec![
            (listed(&made, "fix"), Some(stat)),
            (listed(&outside, "x"), None),
        ]));
        let sent = |rx: &mut UnboundedReceiver<ServerEvent>| {
            let mut out = vec![];
            while let Ok(ServerEvent::Worktrees { list, .. }) = rx.try_recv() {
                out.push(list);
            }
            out
        };
        let lists = sent(&mut rx);
        assert_eq!(lists.len(), 1);
        let list = &lists[0];
        assert_eq!(
            (
                list[0].path.clone(),
                list[0].made_by_termist,
                list[0].shown,
                list[0].stat
            ),
            (made.clone(), true, true, Some(stat))
        );
        assert_eq!(
            (list[1].path.clone(), list[1].made_by_termist, list[1].shown),
            (outside.clone(), false, false),
            "found outside termist: hidden"
        );
        reg.worktrees_scanned(scan(vec![
            (listed(&made, "fix"), Some(stat)),
            (listed(&outside, "x"), None),
        ]));
        assert!(sent(&mut rx).is_empty(), "nothing changed: nothing sent");
        reg.worktrees_scanned(scan(vec![(listed(&made, "fix"), Some(stat))]));
        let lists = sent(&mut rx);
        assert_eq!(lists[0].len(), 1, "the one git no longer lists is gone");
        assert_eq!(reg.store.worktrees().unwrap().len(), 1);
        reg.worktrees_scanned(WorktreeScan {
            project: p.id,
            repo: repo.id,
            found: None,
        });
        assert_eq!(
            reg.store.worktrees().unwrap().len(),
            1,
            "a failed scan forgets nothing"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_worktree_makes_one_beside_the_repo_and_keeps_it_as_termist_s() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir(&site).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        run_git(&site, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: site.clone(),
            open: true,
        };
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let repo = store.upsert_repo(p.id, &site, "acme", "site").unwrap();
        let mut reg = registry_on(store);
        reg.fetch = |_, _| Err("offline".into());
        let mut made = reg.made_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        let ask = |repo, ticket| ClientRequest::CreateWorktree {
            project: p.id,
            repo,
            branch: "fix-login".into(),
            ticket,
        };
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ask(termist_core::github::RepoId(99), 1),
        });
        assert_eq!(
            rx.try_recv().unwrap(),
            ServerEvent::WorktreeNotMade {
                ticket: 1,
                message: "couldn't make a worktree · not loaded yet".into()
            }
        );
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ask(repo.id, 2),
        });
        let m = made.recv().await.unwrap();
        reg.worktree_made(m);
        let want = place::resolved(tmp.path())
            .join("site-worktrees")
            .join("fix-login");
        match rx.try_recv().unwrap() {
            ServerEvent::WorktreeMade {
                ticket, path, note, ..
            } => {
                assert_eq!((ticket, place::resolved(&path)), (2, want.clone()));
                assert_eq!(
                    note.as_deref(),
                    Some("made from local main: no fetch from origin")
                );
            }
            other => panic!("{other:?}"),
        }
        let ServerEvent::Worktrees { list, .. } = rx.try_recv().unwrap() else {
            panic!("the worktrees")
        };
        assert_eq!(list.len(), 1);
        assert!(list[0].made_by_termist && list[0].shown);
        assert_eq!(list[0].base.as_deref(), Some("main"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_worktree_is_removed_only_when_nothing_of_it_would_be_lost_unasked() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir(&site).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        run_git(&site, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let fix = tmp.path().join("site-worktrees").join("fix");
        run_git(
            &site,
            &["worktree", "add", "-q", "-b", "fix", fix.to_str().unwrap()],
        );
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: site.clone(),
            open: true,
        };
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let repo = store.upsert_repo(p.id, &site, "acme", "site").unwrap();
        store
            .upsert_worktree(&StoredWorktree {
                project: p.id,
                repo: Some(repo.id),
                path: fix.clone(),
                branch: Some("fix".into()),
                base: Some("main".into()),
                pr: None,
                made_by_termist: true,
                shown: true,
            })
            .unwrap();
        let mut stopped = stored(&p, "shell-1");
        stopped.cwd = fix.join("src");
        let mut live = stored(&p, "claude-2");
        live.cwd = fix.clone();
        store.upsert_session(&stopped, false).unwrap();
        store.upsert_session(&live, false).unwrap();
        let mut reg = registry_on(store);
        let mut removed = reg.removed_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        let ask = |reg: &mut Registry, path: &Path, force| {
            reg.handle(Msg::Request {
                client: ClientId(1),
                req: ClientRequest::RemoveWorktree {
                    path: path.to_path_buf(),
                    force,
                },
            })
        };
        let failed = |rx: &mut UnboundedReceiver<ServerEvent>| match rx.try_recv() {
            Ok(ServerEvent::RemoveFailed { message, .. }) => message,
            other => panic!("{other:?}"),
        };
        ask(&mut reg, &site, false);
        assert_eq!(failed(&mut rx), "not a worktree termist knows of");
        reg.session_mut(live.id).unwrap().info.status = AgentStatus::Running;
        ask(&mut reg, &fix, false);
        assert_eq!(failed(&mut rx), "stop its cards first");
        reg.session_mut(live.id).unwrap().info.status = AgentStatus::Disconnected;
        std::fs::write(fix.join("notes.txt"), "half done").unwrap();
        ask(&mut reg, &fix, false);
        let r = removed.recv().await.unwrap();
        reg.worktree_removed(r);
        assert_eq!(
            rx.try_recv().unwrap(),
            ServerEvent::RemoveRefused {
                path: fix.clone(),
                files: 1
            }
        );
        assert!(fix.is_dir(), "asked first");
        ask(&mut reg, &fix, true);
        let r = removed.recv().await.unwrap();
        reg.worktree_removed(r);
        let mut events = vec![];
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(events.contains(&ServerEvent::WorktreeRemoved { path: fix.clone() }));
        assert!(!fix.exists(), "the folder is gone");
        let branch = std::process::Command::new("git")
            .arg("-C")
            .arg(&site)
            .args(["branch", "--list", "fix"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&branch.stdout).contains("fix"),
            "the branch stays"
        );
        assert!(reg.store.worktrees().unwrap().is_empty());
        assert!(
            reg.session(stopped.id).unwrap().info.archived,
            "its stopped cards archived"
        );
        assert!(reg.session(live.id).unwrap().info.archived);
    }

    #[test]
    fn a_card_on_a_kept_worktree_names_its_pull_request_for_later() {
        let p = project();
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let repo = store.upsert_repo(p.id, &p.path, "acme", "site").unwrap();
        let path = PathBuf::from("/w/site-worktrees/fix");
        store
            .upsert_worktree(&StoredWorktree {
                project: p.id,
                repo: Some(repo.id),
                path: path.clone(),
                branch: Some("fix".into()),
                base: Some("main".into()),
                pr: None,
                made_by_termist: true,
                shown: true,
            })
            .unwrap();
        let mut reg = registry_on(store);
        let mut card = stored(&p, "claude-1");
        card.place = Some(Box::new(termist_core::Place {
            root: path.clone(),
            branch: Some("fix".into()),
            commit: None,
            repo: Some(repo.id),
            pr: Some(termist_core::github::PrRef {
                repo: repo.id,
                number: 212,
            }),
            gone: false,
        }));
        reg.note_worktree_pr(&card);
        assert_eq!(reg.store.worktrees().unwrap()[0].pr, Some(212));
    }

    #[test]
    fn a_worktree_shown_or_hidden_is_kept_and_sent() {
        let p = project();
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        let path = PathBuf::from("/w/site/.claude/worktrees/x");
        store
            .upsert_worktree(&StoredWorktree {
                project: p.id,
                repo: None,
                path: path.clone(),
                branch: Some("x".into()),
                base: None,
                pr: None,
                made_by_termist: false,
                shown: false,
            })
            .unwrap();
        let mut reg = registry_on(store);
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::SetWorktreeShown {
                path: path.clone(),
                shown: true,
            },
        });
        assert!(reg.store.worktrees().unwrap()[0].shown);
        let Ok(ServerEvent::Worktrees { list, .. }) = rx.try_recv() else {
            panic!("the worktrees again")
        };
        assert!(list[0].shown);
    }

    #[test]
    fn the_folders_are_asked_again_every_round_only_while_someone_looks() {
        let p = project();
        let mut reg = registry_with(&p, &[stored(&p, "shell-1")]);
        let t0 = std::time::Instant::now();
        reg.places_tick(t0);
        assert_eq!(reg.places_round, None, "no client");
        let _rx = connect(&mut reg);
        reg.places_reading.insert(p.path.clone()); // as if on its way: nothing spawned
        reg.places_tick(t0);
        assert_eq!(reg.places_round, Some(t0));
        reg.places_tick(t0 + Duration::from_secs(10));
        assert_eq!(reg.places_round, Some(t0), "not before the round is up");
        reg.places_tick(t0 + PLACES_ROUND);
        assert_eq!(reg.places_round, Some(t0 + PLACES_ROUND));
    }

    /// Runs git in `dir` for a test, as a nameless author; panics when it fails.
    #[cfg(unix)]
    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_starts_in_a_project_folder_or_a_worktree_of_its_repo_and_nowhere_else() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir_all(site.join("src")).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        run_git(&site, &["commit", "-q", "--allow-empty", "-m", "init"]);
        run_git(
            &site,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fix/login",
                "../site-worktrees/fix/login",
            ],
        );
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: site.clone(),
            open: true,
        };
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        store.upsert_repo(p.id, &site, "acme", "site").unwrap();
        let mut reg = registry_on(store);
        reg.launcher
            .programs
            .set(Harness::Claude, "/usr/bin/true".into());
        let claude = || SessionKind::Agent {
            harness: Harness::Claude,
        };
        let start = |reg: &mut Registry, dir: PathBuf| {
            reg.create_session(p.id, claude(), Some(dir), None, None, None, (80, 24))
        };
        let worktree = tmp.path().join("site-worktrees/fix/login");
        start(&mut reg, site.join("src")).unwrap();
        start(&mut reg, worktree.clone()).unwrap();
        let cwds: Vec<PathBuf> = reg.sessions.iter().map(|s| s.info.cwd.clone()).collect();
        assert_eq!(cwds, [site.join("src"), worktree]);
        std::fs::create_dir(tmp.path().join("elsewhere")).unwrap();
        let refused = start(&mut reg, tmp.path().join("elsewhere")).unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("is not a folder of this project")
        );
        let refused = start(&mut reg, tmp.path().join("missing")).unwrap_err();
        assert!(refused.to_string().contains("is not a folder"));
        assert_eq!(reg.sessions.len(), 2, "nothing started for a refusal");
    }

    fn cancelled_by_title(reg: &mut Registry, p: &ProjectInfo) -> SessionId {
        let id = running_claude(reg, p);
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.poll_idle_titles(later(1600));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
        id
    }

    // The idle title can be wrong: a turn that goes on after it undoes the cancel.
    #[test]
    fn a_tool_or_a_stop_after_a_title_cancel_undoes_it() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = cancelled_by_title(&mut reg, &p);
        reg.hook(id, Harness::Claude, "PostToolUse", &Value::Null);
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
        reg.hook(id, Harness::Claude, "Stop", &Value::Null);
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Unseen);

        let id = cancelled_by_title(&mut reg, &p);
        reg.hook(id, Harness::Claude, "Stop", &Value::Null);
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Unseen);
    }

    #[test]
    fn a_working_title_after_a_title_cancel_undoes_it() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = cancelled_by_title(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("◐ Fix login".into())));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Running);
    }

    // An idle or cleared title is no sign that the turn goes on.
    #[test]
    fn an_idle_or_empty_title_after_a_title_cancel_undoes_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = cancelled_by_title(&mut reg, &p);
        reg.note(SessionNote::Title(id, Some("✳ Fix login again".into())));
        reg.note(SessionNote::Title(id, None));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
    }

    #[test]
    fn a_late_hook_after_a_real_cancel_does_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.jsonl");
        std::fs::write(&path, "").unwrap();
        reg.watch_transcript(id, &path);
        std::fs::write(
            &path,
            "{\"message\":{\"content\":\"[Request interrupted by user]\"}}\n",
        )
        .unwrap();
        reg.poll_transcripts();
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
        reg.hook(id, Harness::Claude, "PostToolUse", &Value::Null);
        reg.hook(id, Harness::Claude, "Stop", &Value::Null);
        reg.note(SessionNote::Title(id, Some("◐ Fix login".into())));
        assert_eq!(reg.session(id).unwrap().info.status, AgentStatus::Finished);
    }

    #[test]
    fn an_idle_title_while_waiting_for_the_user_cancels_nothing() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let id = running_claude(&mut reg, &p);
        reg.session_mut(id).unwrap().info.status = AgentStatus::NeedsFeedback;
        reg.note(SessionNote::Title(id, Some("✳ Fix login".into())));
        reg.poll_idle_titles(later(5000));
        assert_eq!(
            reg.session(id).unwrap().info.status,
            AgentStatus::NeedsFeedback
        );
    }

    fn gpt() -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "gpt-6-astra".into(),
            label: "GPT-6-Astra".into(),
            efforts: vec!["low".into()],
        }]
    }

    fn models(rx: &mut UnboundedReceiver<ServerEvent>) -> Vec<(Vec<String>, Vec<ModelInfo>)> {
        let mut out = vec![];
        while let Ok(ev) = rx.try_recv() {
            if let ServerEvent::Models {
                recent, catalog, ..
            } = ev
            {
                out.push((recent, catalog));
            }
        }
        out
    }

    #[tokio::test]
    async fn claude_gets_its_aliases_at_once() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let mut rx = connect(&mut reg);
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::ListModels {
                harness: Harness::Claude,
            },
        });
        let got = models(&mut rx);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, crate::models::claude());
    }

    #[tokio::test]
    async fn a_catalog_is_read_once_an_hour_and_sent_when_it_comes() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.read_catalog = |_, _| gpt();
        let mut rx = connect(&mut reg);
        let mut results = reg.catalog_rx.take().unwrap();
        let t0 = std::time::Instant::now();
        reg.list_models(ClientId(1), Harness::Codex, t0);
        assert_eq!(
            models(&mut rx),
            [(vec![], vec![])],
            "at once, with what there is"
        );
        reg.catalog_read(results.recv().await.unwrap(), t0);
        assert_eq!(models(&mut rx), [(vec![], gpt())], "then the list");
        reg.list_models(ClientId(1), Harness::Codex, t0 + Duration::from_secs(60));
        assert_eq!(models(&mut rx), [(vec![], gpt())]);
        tokio::task::yield_now().await;
        assert!(
            results.try_recv().is_err(),
            "not read again within the hour"
        );
        reg.list_models(ClientId(1), Harness::Codex, t0 + CATALOG_FRESH);
        assert_eq!(
            models(&mut rx),
            [(vec![], gpt())],
            "the old list while the new is read"
        );
        results.recv().await.unwrap();
    }

    // A CLI that listed nothing (missing, or failing) is asked again next time.
    #[tokio::test]
    async fn an_empty_catalog_is_read_again_on_the_next_ask() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.read_catalog = |_, _| vec![];
        let mut rx = connect(&mut reg);
        let mut results = reg.catalog_rx.take().unwrap();
        let t0 = std::time::Instant::now();
        reg.list_models(ClientId(1), Harness::Codex, t0);
        reg.catalog_read(results.recv().await.unwrap(), t0);
        let _ = models(&mut rx);
        reg.list_models(ClientId(1), Harness::Codex, t0 + Duration::from_secs(60));
        tokio::time::timeout(Duration::from_secs(5), results.recv())
            .await
            .expect("read again within the hour")
            .unwrap();
    }

    #[tokio::test]
    async fn a_rescan_that_finds_a_cli_drops_its_cached_catalog() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        let t0 = std::time::Instant::now();
        reg.catalogs.insert(Harness::Codex, (gpt(), t0));
        reg.catalogs.insert(Harness::OpenCode, (gpt(), t0));
        reg.rescanned(Rescanned(vec![(
            Harness::Codex,
            PathBuf::from("/usr/local/bin/codex"),
        )]));
        assert!(!reg.catalogs.contains_key(&Harness::Codex));
        assert!(
            reg.catalogs.contains_key(&Harness::OpenCode),
            "only what the rescan found"
        );
    }

    #[tokio::test]
    async fn two_asks_while_reading_start_one_read() {
        let p = project();
        let mut reg = registry_with(&p, &[]);
        reg.read_catalog = |_, _| gpt();
        let mut rx = connect(&mut reg);
        let mut results = reg.catalog_rx.take().unwrap();
        let t0 = std::time::Instant::now();
        reg.list_models(ClientId(1), Harness::OpenCode, t0);
        reg.list_models(ClientId(1), Harness::OpenCode, t0);
        let found = results.recv().await.unwrap();
        tokio::task::yield_now().await;
        assert!(results.try_recv().is_err(), "one read");
        reg.catalog_read(found, t0);
        let got = models(&mut rx);
        assert_eq!(got.len(), 3, "two answers at once, one with the list");
        assert_eq!(got[2].1, gpt());
    }
}
