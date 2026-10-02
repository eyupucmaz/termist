//! Pull requests from GitHub, read through the `gh` CLI.
//!
//! `GitHub` is a state machine: requests, a clock tick and finished jobs go in; jobs
//! to run and events to send come out (`Effects`). The registry runs the jobs on
//! blocking threads and feeds back what they found (`Done`). Nothing is read while no
//! client is connected or GitHub is off.
pub mod accounts;
pub mod gh;
pub mod poller;
pub mod query;
pub mod repos;

use crate::session::ClientId;
use crate::store::{Store, StoredRepo};
use accounts::{Account, Permission};
use gh::GhHandle;
use poller::Beat;
use repos::LocalRepo;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use termist_core::github::{GhState, PrDetail, PrRef, PrSummary, RepoId, RepoInfo, RepoPrs};
use termist_core::{ClientRequest, ProjectId, ProjectInfo, ServerEvent};

/// Pull requests kept whole, to open one again at once.
#[allow(dead_code)] // read by the rounds of Task 11
const DETAILS: usize = 50;
/// Below this many points left in the hour, an account is read slowly.
#[allow(dead_code)] // read by the rounds of Task 11
const RATE_FLOOR: u32 = 300;

/// A repo to read: its id, owner and name.
pub type Slug = (RepoId, String, String);

/// Per repo: each account's login, its access, and whether it is gh's active one.
pub type Access = Vec<(RepoId, Vec<(String, Option<Permission>, bool)>)>;

#[derive(Debug)]
pub enum Job {
    /// Find gh, its accounts and their tokens.
    Accounts,
    Discover {
        project: ProjectId,
        path: PathBuf,
    },
    /// Each account's access to each repo.
    Permissions {
        gh: GhHandle,
        accounts: Vec<Account>,
        repos: Vec<Slug>,
    },
    Inbox {
        gh: GhHandle,
        project: ProjectId,
        account: Account,
        repos: Vec<Slug>,
    },
    /// Open counts for the repos window, hidden repos too.
    Counts {
        gh: GhHandle,
        project: ProjectId,
        batches: Vec<(Account, Vec<Slug>)>,
    },
    Detail {
        gh: GhHandle,
        pr: PrRef,
        account: Account,
        owner: String,
        name: String,
    },
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // moved once, never stored
pub enum Done {
    Accounts(Result<(GhHandle, Vec<Account>), GhState>),
    Discovered {
        project: ProjectId,
        repos: Vec<LocalRepo>,
    },
    /// Per repo: each account's login, its access, and whether it is gh's active one.
    Permissions(Result<Access, GhState>),
    Inbox {
        project: ProjectId,
        account: String,
        ids: Vec<RepoId>,
        reply: Result<query::InboxReply, GhState>,
    },
    Counts {
        project: ProjectId,
        counts: Vec<(RepoId, Option<u32>)>,
    },
    Detail {
        pr: PrRef,
        reply: Result<PrDetail, GhState>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    All,
    One(ClientId),
}

#[derive(Debug, Default)]
pub struct Effects {
    pub jobs: Vec<Job>,
    pub events: Vec<(To, ServerEvent)>,
}

impl Effects {
    fn send(&mut self, to: To, event: ServerEvent) {
        self.events.push((to, event));
    }

    fn extend(&mut self, other: Effects) {
        self.jobs.extend(other.jobs);
        self.events.extend(other.events);
    }
}

struct Repo {
    stored: StoredRepo,
    /// Found by its project's last discovery; one not found again stays stored.
    present: bool,
    /// The account with the most access, once asked.
    chosen: Option<String>,
    /// Access was asked since the accounts were loaded.
    checked: bool,
    state: GhState,
    viewer: Option<String>,
    prs: Vec<PrSummary>,
    total: u32,
    fetched_at: Option<String>,
    failed_at: Option<String>,
    /// The PRs asking you for a review at the last good read; `None` before it.
    asked: Option<HashSet<u32>>,
    open_count: Option<u32>,
}

impl Repo {
    fn new(stored: StoredRepo) -> Repo {
        Repo {
            stored,
            present: true,
            chosen: None,
            checked: false,
            state: GhState::Ok,
            viewer: None,
            prs: vec![],
            total: 0,
            fetched_at: None,
            failed_at: None,
            asked: None,
            open_count: None,
        }
    }

    /// The account it is read with: the user's, else the chosen one.
    fn account(&self) -> Option<&str> {
        self.stored.account.as_deref().or(self.chosen.as_deref())
    }

    fn slug(&self) -> Slug {
        (
            self.stored.id,
            self.stored.owner.clone(),
            self.stored.name.clone(),
        )
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

enum Auth {
    Unknown,
    Loading,
    Ready {
        gh: GhHandle,
        accounts: Vec<Account>,
    },
    Failed(GhState),
}

/// What a client looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Focus {
    project: Option<ProjectId>,
    pr: Option<PrRef>,
}

#[allow(dead_code)] // detail_beats, details, slow_until, seen: read by Task 11
pub struct GitHub {
    /// The last client's `[github] enabled`.
    enabled: bool,
    clients: HashMap<ClientId, Focus>,
    auth: Auth,
    auth_beat: Beat,
    repos: Vec<Repo>,
    discovered: HashSet<ProjectId>,
    discovering: HashSet<ProjectId>,
    permissions: Beat,
    /// Projects whose repos window waits for open counts.
    counts_wanted: HashSet<ProjectId>,
    /// One inbox round per project and account.
    beats: HashMap<(ProjectId, String), Beat>,
    detail_beats: HashMap<PrRef, Beat>,
    /// Newest last.
    details: Vec<(PrRef, PrDetail)>,
    /// Accounts low on their hourly budget, until when.
    slow_until: HashMap<String, Instant>,
    /// `updatedAt` of each PR when it was last opened.
    seen: HashMap<(String, String, u32), String>,
}

impl GitHub {
    pub fn new(store: &Store) -> GitHub {
        let repos = store.repos().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not load the GitHub repos");
            vec![]
        });
        let seen = store.seen().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not load the pull requests seen");
            HashMap::new()
        });
        GitHub {
            enabled: false,
            clients: HashMap::new(),
            auth: Auth::Unknown,
            auth_beat: Beat::default(),
            repos: repos.into_iter().map(Repo::new).collect(),
            discovered: HashSet::new(),
            discovering: HashSet::new(),
            permissions: Beat::default(),
            counts_wanted: HashSet::new(),
            beats: HashMap::new(),
            detail_beats: HashMap::new(),
            details: vec![],
            slow_until: HashMap::new(),
            seen,
        }
    }

    pub fn connected(&mut self, client: ClientId) {
        self.clients.insert(client, Focus::default());
    }

    pub fn gone(&mut self, client: ClientId) {
        self.clients.remove(&client);
    }

    fn active(&self) -> bool {
        self.enabled && !self.clients.is_empty()
    }

    #[allow(dead_code)] // used by Task 11, and by the tests
    fn repo(&self, id: RepoId) -> Option<&Repo> {
        self.repos.iter().find(|r| r.stored.id == id)
    }

    fn repo_mut(&mut self, id: RepoId) -> Option<&mut Repo> {
        self.repos.iter_mut().find(|r| r.stored.id == id)
    }

    fn ready(&self) -> Option<(GhHandle, Vec<Account>)> {
        match &self.auth {
            Auth::Ready { gh, accounts } => Some((gh.clone(), accounts.clone())),
            _ => None,
        }
    }

    /// The repos found in `project`, by folder.
    fn present(&self, project: ProjectId) -> Vec<&Repo> {
        let mut repos: Vec<&Repo> = self
            .repos
            .iter()
            .filter(|r| r.stored.project == project && r.present)
            .collect();
        repos.sort_by(|a, b| a.stored.path.cmp(&b.stored.path));
        repos
    }

    fn prs_event(&self, project: ProjectId) -> ServerEvent {
        let present = self.present(project);
        let state = match &self.auth {
            Auth::Failed(state) => state.clone(),
            _ => GhState::Ok,
        };
        ServerEvent::Prs {
            project,
            state,
            discovered: present.len() as u32,
            repos: present
                .iter()
                .filter(|r| r.stored.visible)
                .map(|r| RepoPrs {
                    repo: r.stored.id,
                    name: folder_name(&r.stored.path),
                    slug: format!("{}/{}", r.stored.owner, r.stored.name),
                    state: r.state.clone(),
                    viewer: r.viewer.clone(),
                    prs: r.prs.clone(),
                    total: r.total,
                    fetched_at: r.fetched_at.clone(),
                    failed_at: r.failed_at.clone(),
                })
                .collect(),
        }
    }

    fn repos_event(&self, project: ProjectId) -> ServerEvent {
        let accounts = match &self.auth {
            Auth::Ready { accounts, .. } => accounts.iter().map(|a| a.login.clone()).collect(),
            _ => vec![],
        };
        ServerEvent::Repos {
            project,
            accounts,
            repos: self
                .present(project)
                .iter()
                .map(|r| RepoInfo {
                    id: r.stored.id,
                    name: folder_name(&r.stored.path),
                    slug: format!("{}/{}", r.stored.owner, r.stored.name),
                    visible: r.stored.visible,
                    account: r.account().map(str::to_string),
                    pinned: r.stored.account.is_some(),
                    open_count: r.open_count,
                    state: r.state.clone(),
                })
                .collect(),
        }
    }

    /// `Prs` of every open project, to `to`: a client that connects, or a failure
    /// every client must see. A project not looked through yet gets none, so no client
    /// mistakes "not found yet" for "no repo".
    pub fn snapshot(&self, to: To, projects: &[ProjectInfo]) -> Effects {
        let mut fx = Effects::default();
        if self.enabled {
            let failed = matches!(self.auth, Auth::Failed(_));
            for p in projects
                .iter()
                .filter(|p| p.open && (failed || self.discovered.contains(&p.id)))
            {
                fx.send(to, self.prs_event(p.id));
            }
        }
        fx
    }

    fn hurry_project(&mut self, project: ProjectId, now: Instant) {
        for ((p, _), beat) in &mut self.beats {
            if *p == project {
                beat.hurry(now);
            }
        }
    }

    /// The present repos of `project` that `keep` keeps, by the account reading them.
    fn batches(
        &self,
        project: ProjectId,
        accounts: &[Account],
        keep: impl Fn(&Repo) -> bool,
    ) -> Vec<(Account, Vec<Slug>)> {
        let mut by: BTreeMap<&str, Vec<Slug>> = BTreeMap::new();
        for r in self.present(project).into_iter().filter(|r| keep(r)) {
            if let Some(login) = r.account() {
                by.entry(login).or_default().push(r.slug());
            }
        }
        by.into_iter()
            .filter_map(|(login, slugs)| {
                Some((accounts.iter().find(|a| a.login == login)?.clone(), slugs))
            })
            .collect()
    }

    pub fn request(
        &mut self,
        client: ClientId,
        req: ClientRequest,
        store: &Store,
        projects: &[ProjectInfo],
        now: Instant,
    ) -> Effects {
        let mut fx = Effects::default();
        if let ClientRequest::SetGitHub { enabled } = req {
            let was = self.enabled;
            self.enabled = enabled;
            if enabled && !was {
                fx.extend(self.snapshot(To::All, projects));
            }
            return fx;
        }
        if !self.enabled {
            return fx;
        }
        match req {
            ClientRequest::ListRepos { project } => {
                fx.send(To::One(client), self.repos_event(project));
                if let Some(p) = projects.iter().find(|p| p.id == project)
                    && self.discovering.insert(project)
                {
                    fx.jobs.push(Job::Discover {
                        project,
                        path: p.path.clone(),
                    });
                }
                self.counts_wanted.insert(project);
            }
            ClientRequest::SetRepoVisible { repo, visible } => {
                if let Err(e) = store.set_repo_visible(repo, visible) {
                    tracing::warn!(error = %e, "could not store a repo's visibility");
                }
                let Some(r) = self.repo_mut(repo) else {
                    return fx;
                };
                r.stored.visible = visible;
                let project = r.stored.project;
                self.hurry_project(project, now);
                fx.send(To::All, self.repos_event(project));
                fx.send(To::All, self.prs_event(project));
            }
            ClientRequest::SetRepoAccount { repo, account } => {
                if let Err(e) = store.set_repo_account(repo, account.as_deref()) {
                    tracing::warn!(error = %e, "could not store a repo's account");
                }
                let Some(r) = self.repo_mut(repo) else {
                    return fx;
                };
                let auto = account.is_none();
                r.stored.account = account;
                r.state = GhState::Ok;
                // Another account is another viewer: its first read announces nothing.
                r.asked = None;
                if auto {
                    r.checked = false;
                    r.chosen = None;
                }
                let project = r.stored.project;
                if auto {
                    self.permissions.hurry(now);
                }
                self.hurry_project(project, now);
                fx.send(To::All, self.repos_event(project));
                fx.send(To::All, self.prs_event(project));
            }
            _ => {}
        }
        fx
    }

    pub fn tick(&mut self, now: Instant, projects: &[ProjectInfo]) -> Effects {
        let mut fx = Effects::default();
        if !self.active() {
            return fx;
        }
        if matches!(self.auth, Auth::Unknown | Auth::Failed(_)) && self.auth_beat.due(now) {
            self.auth_beat.start();
            self.auth = Auth::Loading;
            fx.jobs.push(Job::Accounts);
        }
        for p in projects.iter().filter(|p| p.open) {
            if !self.discovered.contains(&p.id) && self.discovering.insert(p.id) {
                fx.jobs.push(Job::Discover {
                    project: p.id,
                    path: p.path.clone(),
                });
            }
        }
        let Some((gh, accounts)) = self.ready() else {
            return fx;
        };
        let ask: Vec<Slug> = self
            .repos
            .iter()
            .filter(|r| r.present && !r.checked && r.stored.account.is_none())
            .map(Repo::slug)
            .collect();
        if !ask.is_empty() && self.permissions.due(now) {
            self.permissions.start();
            fx.jobs.push(Job::Permissions {
                gh: gh.clone(),
                accounts: accounts.clone(),
                repos: ask,
            });
        }
        let wanted: Vec<ProjectId> = self
            .counts_wanted
            .iter()
            .copied()
            .filter(|p| !self.discovering.contains(p))
            .collect();
        for project in wanted {
            let unchecked = self
                .present(project)
                .iter()
                .any(|r| r.account().is_none() && !r.checked);
            if unchecked {
                continue;
            }
            self.counts_wanted.remove(&project);
            let batches = self.batches(project, &accounts, |_| true);
            if !batches.is_empty() {
                fx.jobs.push(Job::Counts {
                    gh: gh.clone(),
                    project,
                    batches,
                });
            }
        }
        fx
    }

    pub fn done(
        &mut self,
        done: Done,
        now: Instant,
        store: &Store,
        projects: &[ProjectInfo],
    ) -> Effects {
        let mut fx = Effects::default();
        match done {
            Done::Accounts(Ok((gh, accounts))) => {
                self.auth_beat.finish(now, true, poller::AUTH_RETRY);
                for r in &mut self.repos {
                    r.checked = false;
                    r.chosen = None;
                    r.state = match &r.stored.account {
                        Some(login) if !accounts.iter().any(|a| &a.login == login) => {
                            GhState::LoggedOut
                        }
                        _ => GhState::Ok,
                    };
                }
                self.permissions = Beat::default();
                self.auth = Auth::Ready { gh, accounts };
            }
            Done::Accounts(Err(state)) => {
                self.auth_beat.finish(now, false, poller::AUTH_RETRY);
                self.auth = Auth::Failed(state);
                fx.extend(self.snapshot(To::All, projects));
            }
            Done::Discovered { project, repos } => {
                self.discovering.remove(&project);
                self.discovered.insert(project);
                let mut found = HashSet::new();
                for local in repos {
                    let stored =
                        match store.upsert_repo(project, &local.path, &local.owner, &local.repo) {
                            Ok(stored) => stored,
                            Err(e) => {
                                tracing::warn!(error = %e, "could not store a GitHub repo");
                                continue;
                            }
                        };
                    found.insert(stored.id);
                    match self.repos.iter_mut().find(|r| r.stored.id == stored.id) {
                        Some(r) => {
                            if (&r.stored.owner, &r.stored.name) != (&stored.owner, &stored.name) {
                                r.checked = false;
                                r.chosen = None;
                                r.asked = None;
                            }
                            r.stored = stored;
                        }
                        None => self.repos.push(Repo::new(stored)),
                    }
                }
                for r in self
                    .repos
                    .iter_mut()
                    .filter(|r| r.stored.project == project)
                {
                    r.present = found.contains(&r.stored.id);
                }
                self.permissions.hurry(now);
                fx.send(To::All, self.repos_event(project));
                fx.send(To::All, self.prs_event(project));
            }
            Done::Permissions(Ok(results)) => {
                self.permissions.finish(now, true, poller::PERMISSIONS);
                let mut touched = HashSet::new();
                for (id, seen) in results {
                    let Some(r) = self.repo_mut(id) else {
                        continue;
                    };
                    r.checked = true;
                    r.chosen = accounts::pick(&seen);
                    if r.account().is_none() {
                        r.state = GhState::NoAccess;
                    }
                    touched.insert(r.stored.project);
                }
                // Repos found while the question was on its way.
                if self
                    .repos
                    .iter()
                    .any(|r| r.present && !r.checked && r.stored.account.is_none())
                {
                    self.permissions.hurry(now);
                }
                for project in touched {
                    fx.send(To::All, self.repos_event(project));
                    fx.send(To::All, self.prs_event(project));
                }
            }
            Done::Permissions(Err(_)) => self.permissions.finish(now, false, poller::PERMISSIONS),
            Done::Counts { project, counts } => {
                for (id, n) in counts {
                    if let Some(r) = self.repo_mut(id) {
                        r.open_count = n;
                    }
                }
                fx.send(To::All, self.repos_event(project));
            }
            Done::Inbox { .. } | Done::Detail { .. } => {}
        }
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::accounts::Permission::*;
    use super::gh::fake::FakeGh;
    use super::*;

    fn project(name: &str) -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(),
            name: name.into(),
            path: PathBuf::from(format!("/code/{name}")),
            open: true,
        }
    }

    /// The jobs never run in these tests; the handle only travels.
    fn handle() -> GhHandle {
        GhHandle(FakeGh::new(|_| Err(GhState::NoGh)))
    }

    fn account(login: &str, active: bool) -> Account {
        Account {
            login: login.into(),
            active,
            token: format!("tok-{login}"),
        }
    }

    struct World {
        gh: GitHub,
        store: Store,
        projects: Vec<ProjectInfo>,
        now: Instant,
        client: ClientId,
    }

    fn world(names: &[&str]) -> World {
        let store = Store::open_in_memory();
        let projects: Vec<ProjectInfo> = names.iter().map(|n| project(n)).collect();
        for p in &projects {
            store.upsert_project(p).unwrap();
        }
        World {
            gh: GitHub::new(&store),
            store,
            projects,
            now: Instant::now(),
            client: ClientId(1),
        }
    }

    impl World {
        fn request(&mut self, req: ClientRequest) -> Effects {
            self.gh
                .request(self.client, req, &self.store, &self.projects, self.now)
        }

        fn tick(&mut self) -> Effects {
            self.gh.tick(self.now, &self.projects)
        }

        fn done(&mut self, done: Done) -> Effects {
            self.gh.done(done, self.now, &self.store, &self.projects)
        }

        /// A client connected, GitHub on, these accounts loaded.
        fn ready(&mut self, accounts: Vec<Account>) {
            self.gh.connected(self.client);
            self.request(ClientRequest::SetGitHub { enabled: true });
            let fx = self.tick();
            assert!(matches!(fx.jobs.first(), Some(Job::Accounts)));
            self.done(Done::Accounts(Ok((handle(), accounts))));
        }

        /// Project `i` was found to hold these repos: (folder, owner, name).
        fn found(&mut self, i: usize, repos: &[(&str, &str, &str)]) -> Effects {
            let p = &self.projects[i];
            let repos = repos
                .iter()
                .map(|(dir, owner, repo)| LocalRepo {
                    path: p.path.join(dir),
                    name: dir.to_string(),
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                })
                .collect();
            let project = p.id;
            self.done(Done::Discovered { project, repos })
        }

        fn id(&self, name: &str) -> RepoId {
            self.gh
                .repos
                .iter()
                .find(|r| r.stored.name == name)
                .unwrap()
                .stored
                .id
        }

        /// Answers the permission question the next tick asks.
        fn permit(&mut self, answers: &[Answer]) {
            let fx = self.tick();
            assert!(
                fx.jobs.iter().any(|j| matches!(j, Job::Permissions { .. })),
                "{:?}",
                fx.jobs
            );
            let results = answers
                .iter()
                .map(|(name, seen)| {
                    (
                        self.id(name),
                        seen.iter()
                            .map(|(l, p, a)| (l.to_string(), *p, *a))
                            .collect(),
                    )
                })
                .collect();
            self.done(Done::Permissions(Ok(results)));
        }
    }

    /// One answer of the permission question: a folder, then what each account sees.
    type Answer<'a> = (&'a str, &'a [(&'a str, Option<Permission>, bool)]);

    /// The repos of the `Repos` events among `fx`: (folder, account, visible).
    fn listed(fx: &Effects) -> Vec<(String, Option<String>, bool)> {
        fx.events
            .iter()
            .rev()
            .find_map(|(_, e)| match e {
                ServerEvent::Repos { repos, .. } => Some(
                    repos
                        .iter()
                        .map(|r| (r.name.clone(), r.account.clone(), r.visible))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The last `Prs` among `fx`: (state, discovered, shown folders).
    fn shown(fx: &Effects) -> Option<(GhState, u32, Vec<String>)> {
        fx.events.iter().rev().find_map(|(_, e)| match e {
            ServerEvent::Prs {
                state,
                discovered,
                repos,
                ..
            } => Some((
                state.clone(),
                *discovered,
                repos.iter().map(|r| r.name.clone()).collect(),
            )),
            _ => None,
        })
    }

    #[test]
    fn nothing_runs_until_a_client_turns_github_on() {
        let mut w = world(&["work"]);
        assert!(w.tick().jobs.is_empty(), "no client");
        w.gh.connected(w.client);
        assert!(w.tick().jobs.is_empty(), "GitHub off");
        w.request(ClientRequest::SetGitHub { enabled: true });
        let fx = w.tick();
        assert!(
            matches!(fx.jobs[..], [Job::Accounts, Job::Discover { .. }]),
            "{:?}",
            fx.jobs
        );
        assert!(w.tick().jobs.is_empty(), "nothing twice");
    }

    #[test]
    fn failed_accounts_tell_every_client_why_and_wait() {
        let mut w = world(&["work"]);
        w.gh.connected(w.client);
        w.request(ClientRequest::SetGitHub { enabled: true });
        w.tick();
        let fx = w.done(Done::Accounts(Err(GhState::NoGh)));
        assert_eq!(fx.events[0].0, To::All);
        assert_eq!(shown(&fx), Some((GhState::NoGh, 0, vec![])));
        assert!(w.tick().jobs.is_empty(), "not again at once");
        w.now += poller::AUTH_RETRY;
        assert!(matches!(w.tick().jobs[..], [Job::Accounts]));
    }

    #[test]
    fn discovered_repos_are_stored_and_listed() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true)]);
        let fx = w.found(0, &[("site", "acme", "site"), ("admin", "acme", "admin")]);
        assert_eq!(
            listed(&fx),
            [
                ("admin".to_string(), None, true),
                ("site".to_string(), None, true)
            ]
        );
        assert_eq!(shown(&fx).unwrap().1, 2);
        assert_eq!(w.store.repos().unwrap().len(), 2);
        let fx = w.found(0, &[("site", "acme", "site")]);
        assert_eq!(
            shown(&fx).unwrap(),
            (GhState::Ok, 1, vec!["site".to_string()])
        );
        assert_eq!(
            w.store.repos().unwrap().len(),
            2,
            "a repo not found again stays stored"
        );
    }

    #[test]
    fn permissions_pick_the_account_with_the_most_access() {
        let mut w = world(&["work", "termist"]);
        w.ready(vec![account("work", true), account("me", false)]);
        w.found(0, &[("site", "acme", "site"), ("lab", "acme", "lab")]);
        w.found(1, &[("termist", "me", "termist")]);
        w.permit(&[
            ("site", &[("work", Some(Write), true), ("me", None, false)]),
            ("lab", &[("work", None, true), ("me", None, false)]),
            (
                "termist",
                &[("work", Some(Read), true), ("me", Some(Admin), false)],
            ),
        ]);
        let fx = w.request(ClientRequest::ListRepos {
            project: w.projects[0].id,
        });
        assert_eq!(
            listed(&fx),
            [
                ("lab".to_string(), None, true),
                ("site".to_string(), Some("work".into()), true)
            ]
        );
        let r = w.gh.repo(w.id("lab")).unwrap();
        assert_eq!(r.state, GhState::NoAccess);
        assert_eq!(w.gh.repo(w.id("termist")).unwrap().account(), Some("me"));
    }

    #[test]
    fn a_pinned_account_wins_and_auto_asks_again() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true), account("me", false)]);
        w.found(0, &[("site", "acme", "site")]);
        w.permit(&[(
            "site",
            &[("work", Some(Write), true), ("me", Some(Read), false)],
        )]);
        let site = w.id("site");
        let fx = w.request(ClientRequest::SetRepoAccount {
            repo: site,
            account: Some("me".into()),
        });
        assert_eq!(listed(&fx), [("site".to_string(), Some("me".into()), true)]);
        assert_eq!(w.store.repos().unwrap()[0].account.as_deref(), Some("me"));
        w.request(ClientRequest::SetRepoAccount {
            repo: site,
            account: None,
        });
        w.permit(&[(
            "site",
            &[("work", Some(Write), true), ("me", Some(Read), false)],
        )]);
        assert_eq!(w.gh.repo(site).unwrap().account(), Some("work"));
    }

    #[test]
    fn hidden_repos_are_listed_but_not_shown() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true)]);
        w.found(0, &[("site", "acme", "site"), ("admin", "acme", "admin")]);
        let fx = w.request(ClientRequest::SetRepoVisible {
            repo: w.id("admin"),
            visible: false,
        });
        assert_eq!(
            shown(&fx).unwrap(),
            (GhState::Ok, 2, vec!["site".to_string()])
        );
        assert_eq!(
            listed(&fx),
            [
                ("admin".to_string(), None, false),
                ("site".to_string(), None, true)
            ]
        );
        assert!(
            !w.store
                .repos()
                .unwrap()
                .iter()
                .find(|r| r.name == "admin")
                .unwrap()
                .visible
        );
    }

    #[test]
    fn the_repos_window_gets_counts_for_every_repo() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true)]);
        w.found(0, &[("site", "acme", "site"), ("admin", "acme", "admin")]);
        w.request(ClientRequest::SetRepoVisible {
            repo: w.id("admin"),
            visible: false,
        });
        let project = w.projects[0].id;
        let fx = w.request(ClientRequest::ListRepos { project });
        assert!(matches!(
            fx.events[0],
            (To::One(_), ServerEvent::Repos { .. })
        ));
        assert!(matches!(fx.jobs[..], [Job::Discover { .. }]));
        w.found(0, &[("site", "acme", "site"), ("admin", "acme", "admin")]);
        w.permit(&[
            ("site", &[("work", Some(Write), true)]),
            ("admin", &[("work", Some(Write), true)]),
        ]);
        let fx = w.tick();
        let batches = fx
            .jobs
            .iter()
            .find_map(|j| match j {
                Job::Counts { batches, .. } => Some(batches.clone()),
                _ => None,
            })
            .expect("a counts job");
        assert_eq!(batches[0].1.len(), 2, "hidden repos are counted too");
        let fx = w.done(Done::Counts {
            project,
            counts: vec![(w.id("site"), Some(3)), (w.id("admin"), Some(1))],
        });
        let ServerEvent::Repos { repos, .. } = &fx.events[0].1 else {
            panic!()
        };
        let counts: Vec<_> = repos.iter().map(|r| r.open_count).collect();
        assert_eq!(counts, [Some(1), Some(3)]);
    }
}
