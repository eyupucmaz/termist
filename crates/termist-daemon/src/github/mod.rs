//! Pull requests from GitHub, read through the `gh` CLI.
//!
//! `GitHub` is a state machine: requests, a clock tick and finished jobs go in; jobs
//! to run and events to send come out (`Effects`). The registry runs the jobs on
//! blocking threads and feeds back what they found (`Done`). Nothing is read while no
//! client is connected or GitHub is off.
pub mod accounts;
pub mod files;
pub mod gh;
pub mod jobs;
pub mod poller;
pub mod query;
pub mod repos;
pub mod write;

use crate::session::ClientId;
use crate::store::{Store, StoredRepo};
use accounts::{Account, Permission};
use gh::GhHandle;
use poller::Beat;
use repos::LocalRepo;
use std::collections::VecDeque;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use termist_core::github::{
    GhState, PrDetail, PrDiff, PrRef, PrSummary, PrWrite, RepoId, RepoInfo, RepoPrs, Viewed,
    rfc3339,
};
use termist_core::status::now_ms;
use termist_core::{ClientRequest, ProjectId, ProjectInfo, ServerEvent};

/// Pull requests kept whole, to open one again at once.
const DETAILS: usize = 50;
/// Diffs kept, one per pull request: the newest head's.
const DIFFS: usize = 10;
/// Below this many points left in the hour, an account is read slowly.
const RATE_FLOOR: u32 = 300;
/// A folder with more repos than this shows only its first ones at first; the repos
/// window shows the others. Every shown repo spends the hourly budget.
const SHOWN_AT_FIRST: usize = 10;

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
    /// The files and patches of one pull request at one head commit.
    Diff {
        gh: GhHandle,
        pr: PrRef,
        account: Account,
        want: files::Want,
    },
    /// Writes to a pull request, as `client` asked under `ticket`.
    Write {
        gh: GhHandle,
        pr: PrRef,
        account: Account,
        at: write::Spot,
        write: PrWrite,
        client: ClientId,
        ticket: u64,
    },
    /// Marks a file viewed on GitHub, or not, as `client` asked.
    MarkViewed {
        gh: GhHandle,
        pr: PrRef,
        account: Account,
        id: String,
        path: String,
        viewed: bool,
        client: ClientId,
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
    /// Looking through the project failed: what was stored stays.
    Undiscovered {
        project: ProjectId,
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
    Diff {
        pr: PrRef,
        head_oid: String,
        reply: Result<PrDiff, GhState>,
    },
    Written {
        pr: PrRef,
        client: ClientId,
        ticket: u64,
        /// What it was, for "couldn't …".
        what: &'static str,
        reply: Result<(), GhState>,
    },
    Marked {
        pr: PrRef,
        path: String,
        viewed: bool,
        client: ClientId,
        reply: Result<(), GhState>,
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

    /// Picks the account to read with; another reader is another viewer, whose first
    /// read announces nothing.
    fn choose(&mut self, chosen: Option<String>) {
        let before = self.account().map(str::to_string);
        self.chosen = chosen;
        if self.account() != before.as_deref() {
            self.asked = None;
        }
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
    /// The diff of `pr`.
    diff: bool,
}

/// A failure in a few words, for a message after "couldn't …".
fn said(state: &GhState) -> String {
    match state {
        GhState::Ok => "it did not happen".into(),
        GhState::NoGh => "gh is not installed".into(),
        GhState::LoggedOut => "gh is not logged in".into(),
        GhState::NoAccess => "no access".into(),
        GhState::RateLimited { .. } => "GitHub rate limit".into(),
        GhState::Failed(why) => why.clone(),
    }
}

/// The last part of a path, as a message names a file.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub struct GitHub {
    /// The last client's `[github] enabled`.
    enabled: bool,
    clients: HashMap<ClientId, Focus>,
    auth: Auth,
    auth_beat: Beat,
    /// Load the accounts again while the old ones keep reading (`R`, the repos window).
    reload: bool,
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
    /// One diff per pull request, newest last.
    diffs: Vec<(PrRef, PrDiff)>,
    /// Diffs being read.
    diff_flying: HashSet<PrRef>,
    /// The head whose diff could not be read: not tried again until a refresh.
    diff_failed: HashMap<PrRef, String>,
    /// Diffs to read again although read at this head (`R`); the one read stays
    /// shown until the new one comes.
    diff_stale: HashSet<PrRef>,
    /// Writes waiting, per pull request: one runs at a time, so two line comments
    /// never open two pending reviews.
    writes: HashMap<PrRef, VecDeque<(ClientId, u64, PrWrite)>>,
    writing: HashSet<PrRef>,
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
            reload: false,
            repos: repos.into_iter().map(Repo::new).collect(),
            discovered: HashSet::new(),
            discovering: HashSet::new(),
            permissions: Beat::default(),
            counts_wanted: HashSet::new(),
            beats: HashMap::new(),
            detail_beats: HashMap::new(),
            details: vec![],
            diffs: vec![],
            diff_flying: HashSet::new(),
            diff_failed: HashMap::new(),
            diff_stale: HashSet::new(),
            writes: HashMap::new(),
            writing: HashSet::new(),
            slow_until: HashMap::new(),
            seen,
        }
    }

    pub fn gone(&mut self, client: ClientId) {
        self.clients.remove(&client);
    }

    fn active(&self) -> bool {
        self.enabled && !self.clients.is_empty()
    }

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

    /// Accounts and access may have changed outside termist (a `gh auth login`, an
    /// invitation accepted): load the accounts again and ask again about the repos of
    /// `project` no account could see.
    fn look_again(&mut self, project: ProjectId, now: Instant) {
        if matches!(self.auth, Auth::Ready { .. }) {
            self.reload = true;
        }
        for r in self
            .repos
            .iter_mut()
            .filter(|r| r.stored.project == project && r.state == GhState::NoAccess)
        {
            r.checked = false;
        }
        self.permissions.hurry(now);
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
            // A client counts once it says how it wants GitHub: the TUI does at once,
            // the short-lived hook connections never do.
            self.clients.entry(client).or_default();
            let was = self.enabled;
            self.enabled = enabled;
            if enabled && !was {
                fx.extend(self.snapshot(To::All, projects));
            }
            return fx;
        }
        if let ClientRequest::SetPrFocus { project, pr, diff } = req {
            // Only a client that is still known: a late request after `gone` is ignored.
            let Some(focus) = self.clients.get_mut(&client) else {
                return fx;
            };
            let before = std::mem::replace(focus, Focus { project, pr, diff });
            if let Some(p) = project
                && before.project != Some(p)
            {
                for ((bp, _), beat) in &mut self.beats {
                    if *bp == p {
                        beat.freshen(now, poller::FOCUSED);
                        beat.cap(now, poller::FOCUSED);
                    }
                }
            }
            if let Some(pr) = pr
                && before.pr != Some(pr)
            {
                if let Some(detail) = self.cached(pr) {
                    fx.send(
                        To::One(client),
                        ServerEvent::PrDetail {
                            pr,
                            state: GhState::Ok,
                            detail: Some(Box::new(detail.clone())),
                        },
                    );
                }
                self.detail_beats.entry(pr).or_default().hurry(now);
            }
            if let Some(pr) = pr
                && diff
                && (before.pr != Some(pr) || !before.diff)
                && let Some(d) = self.cached_diff(pr)
            {
                fx.send(
                    To::One(client),
                    ServerEvent::PrDiff {
                        pr,
                        state: GhState::Ok,
                        diff: Some(Box::new(d.clone())),
                    },
                );
            }
            return fx;
        }
        if !self.enabled {
            return fx;
        }
        match req {
            ClientRequest::ListRepos { project } => {
                self.look_again(project, now);
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
            ClientRequest::RefreshPrs { project } => {
                if matches!(self.auth, Auth::Failed(_)) {
                    self.auth_beat.hurry(now);
                }
                self.look_again(project, now);
                self.hurry_project(project, now);
                let open: Vec<(PrRef, bool)> = self
                    .clients
                    .values()
                    .filter_map(|f| Some((f.pr?, f.diff)))
                    .collect();
                for (pr, diff) in open {
                    if let Some(beat) = self.detail_beats.get_mut(&pr) {
                        beat.hurry(now);
                    }
                    if diff {
                        // Read again even at the same head.
                        self.diff_failed.remove(&pr);
                        self.diff_stale.insert(pr);
                    }
                }
            }
            ClientRequest::WritePr { pr, ticket, write } => {
                let known =
                    self.cached(pr).is_some_and(|d| !d.id.is_empty()) && self.ready().is_some();
                if !known {
                    fx.send(
                        To::One(client),
                        ServerEvent::PrWriteFailed {
                            pr,
                            ticket: Some(ticket),
                            message: format!("couldn't {} · not loaded yet", write::what(&write)),
                        },
                    );
                    return fx;
                }
                self.writes
                    .entry(pr)
                    .or_default()
                    .push_back((client, ticket, write));
                self.next_write(pr, &mut fx);
            }
            ClientRequest::SetFileViewed { pr, path, viewed } => {
                let job = (|| {
                    let (gh, accounts) = self.ready()?;
                    let id = self
                        .cached(pr)
                        .map(|d| d.id.clone())
                        .filter(|id| !id.is_empty())?;
                    let login = self.repo(pr.repo)?.account()?;
                    let account = accounts.into_iter().find(|a| a.login == login)?;
                    Some(Job::MarkViewed {
                        gh,
                        pr,
                        account,
                        id,
                        path: path.clone(),
                        viewed,
                        client,
                    })
                })();
                match job {
                    Some(job) => fx.jobs.push(job),
                    None => fx.send(
                        To::One(client),
                        ServerEvent::PrWriteFailed {
                            pr,
                            ticket: None,
                            message: format!(
                                "couldn't mark {} {} · not loaded yet",
                                file_name(&path),
                                if viewed { "viewed" } else { "unviewed" }
                            ),
                        },
                    ),
                }
            }
            ClientRequest::MarkPrSeen { pr, updated_at } => {
                let Some(r) = self.repos.iter_mut().find(|r| r.stored.id == pr.repo) else {
                    return fx;
                };
                let key = (r.stored.owner.clone(), r.stored.name.clone(), pr.number);
                if let Err(e) = store.mark_seen(&key.0, &key.1, key.2, &updated_at) {
                    tracing::warn!(error = %e, "could not store a pull request as seen");
                }
                if let Some(p) = r.prs.iter_mut().find(|p| p.number == pr.number)
                    && p.updated_at <= updated_at
                {
                    p.unseen = false;
                }
                let project = r.stored.project;
                self.seen.insert(key, updated_at);
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
        } else if self.reload
            && matches!(self.auth, Auth::Ready { .. })
            && !self.auth_beat.in_flight()
        {
            // Ready stays, with the old accounts, until the new list arrives.
            self.reload = false;
            self.auth_beat.start();
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
        fx.extend(self.rounds(now, projects, &gh, &accounts));
        fx
    }

    fn cached(&self, pr: PrRef) -> Option<&PrDetail> {
        self.details.iter().find(|(p, _)| *p == pr).map(|(_, d)| d)
    }

    /// Starts the next write waiting on `pr`, unless one is running. One that cannot
    /// start (the accounts went, the repo lost its reader) is answered at once and the
    /// next one tried.
    fn next_write(&mut self, pr: PrRef, fx: &mut Effects) {
        if self.writing.contains(&pr) {
            return;
        }
        while let Some((client, ticket, write)) =
            self.writes.get_mut(&pr).and_then(VecDeque::pop_front)
        {
            let job = (|| {
                let (gh, accounts) = self.ready()?;
                let detail = self.cached(pr).filter(|d| !d.id.is_empty())?;
                let r = self.repo(pr.repo)?;
                let login = r.account()?;
                let account = accounts.into_iter().find(|a| a.login == login)?;
                Some(Job::Write {
                    gh,
                    pr,
                    account,
                    at: write::Spot {
                        id: detail.id.clone(),
                        owner: r.stored.owner.clone(),
                        name: r.stored.name.clone(),
                        number: pr.number,
                    },
                    write: write.clone(),
                    client,
                    ticket,
                })
            })();
            match job {
                Some(job) => {
                    self.writing.insert(pr);
                    fx.jobs.push(job);
                    return;
                }
                None => fx.send(
                    To::One(client),
                    ServerEvent::PrWriteFailed {
                        pr,
                        ticket: Some(ticket),
                        message: format!("couldn't {} · not loaded yet", write::what(&write)),
                    },
                ),
            }
        }
        self.writes.remove(&pr);
    }

    fn cached_diff(&self, pr: PrRef) -> Option<&PrDiff> {
        self.diffs.iter().find(|(p, _)| *p == pr).map(|(_, d)| d)
    }

    fn cache_diff(&mut self, pr: PrRef, diff: PrDiff) {
        self.diffs.retain(|(p, _)| *p != pr);
        self.diffs.push((pr, diff));
        if self.diffs.len() > DIFFS {
            self.diffs.remove(0);
        }
    }

    /// `event` to every client looking at the diff of `pr`.
    fn to_diff_watchers(&self, pr: PrRef, event: ServerEvent, fx: &mut Effects) {
        for (client, focus) in &self.clients {
            if focus.pr == Some(pr) && focus.diff {
                fx.send(To::One(*client), event.clone());
            }
        }
    }

    /// The diffs looked at whose head is known and not read yet.
    fn diff_jobs(&mut self, gh: &GhHandle, accounts: &[Account]) -> Vec<Job> {
        let wanted: HashSet<PrRef> = self
            .clients
            .values()
            .filter(|f| f.diff)
            .filter_map(|f| f.pr)
            .collect();
        let mut jobs = Vec::new();
        for pr in wanted {
            // The head comes with the detail: no detail, no diff yet.
            let Some(detail) = self.cached(pr) else {
                continue;
            };
            let head = &detail.head_oid;
            let read = !self.diff_stale.contains(&pr)
                && self.cached_diff(pr).is_some_and(|d| d.head_oid == *head);
            if read || self.diff_flying.contains(&pr) || self.diff_failed.get(&pr) == Some(head) {
                continue;
            }
            let Some(r) = self.repo(pr.repo) else {
                continue;
            };
            let Some(account) = r
                .account()
                .and_then(|login| accounts.iter().find(|a| a.login == login))
            else {
                continue;
            };
            jobs.push(Job::Diff {
                gh: gh.clone(),
                pr,
                account: account.clone(),
                want: files::Want {
                    owner: r.stored.owner.clone(),
                    name: r.stored.name.clone(),
                    number: pr.number,
                    url: detail.summary.url.clone(),
                    changed: detail.summary.changed_files,
                    head_oid: head.clone(),
                },
            });
        }
        for job in &jobs {
            if let Job::Diff { pr, .. } = job {
                self.diff_flying.insert(*pr);
            }
        }
        jobs
    }

    fn cache(&mut self, pr: PrRef, detail: PrDetail) {
        self.details.retain(|(p, _)| *p != pr);
        self.details.push((pr, detail));
        if self.details.len() > DETAILS {
            self.details.remove(0);
        }
    }

    /// The inbox rounds that are due, and the open pull requests' details.
    fn rounds(
        &mut self,
        now: Instant,
        projects: &[ProjectInfo],
        gh: &GhHandle,
        accounts: &[Account],
    ) -> Effects {
        let mut fx = Effects::default();
        for p in projects.iter().filter(|p| p.open) {
            for (account, repos) in self.batches(p.id, accounts, |r| r.stored.visible) {
                let beat = self.beats.entry((p.id, account.login.clone())).or_default();
                if beat.due(now) {
                    beat.start();
                    fx.jobs.push(Job::Inbox {
                        gh: gh.clone(),
                        project: p.id,
                        account,
                        repos,
                    });
                }
            }
        }
        let open: HashSet<PrRef> = self.clients.values().filter_map(|f| f.pr).collect();
        self.detail_beats.retain(|pr, _| open.contains(pr));
        for pr in open {
            if self.detail_beats.get(&pr).is_some_and(|b| !b.due(now)) {
                continue;
            }
            let Some(r) = self.repo(pr.repo) else {
                continue;
            };
            let Some(account) = r
                .account()
                .and_then(|login| accounts.iter().find(|a| a.login == login))
            else {
                continue;
            };
            let job = Job::Detail {
                gh: gh.clone(),
                pr,
                account: account.clone(),
                owner: r.stored.owner.clone(),
                name: r.stored.name.clone(),
            };
            self.detail_beats.entry(pr).or_default().start();
            fx.jobs.push(job);
        }
        fx.jobs.extend(self.diff_jobs(gh, accounts));
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
                    // A reload keeps the reader while it is still logged in, until
                    // access is asked again.
                    let kept = r
                        .chosen
                        .clone()
                        .filter(|login| accounts.iter().any(|a| &a.login == login));
                    r.choose(kept);
                    r.state = match &r.stored.account {
                        Some(login) if !accounts.iter().any(|a| &a.login == login) => {
                            GhState::LoggedOut
                        }
                        _ => GhState::Ok,
                    };
                }
                self.permissions = Beat::default();
                self.auth = Auth::Ready { gh, accounts };
                fx.extend(self.snapshot(To::All, projects));
            }
            Done::Accounts(Err(GhState::Failed(why)))
                if matches!(self.auth, Auth::Ready { .. }) =>
            {
                // A reload that could not finish: the accounts read so far still work.
                tracing::warn!(error = %why, "could not load the GitHub accounts again");
                self.auth_beat.finish(now, false, poller::AUTH_RETRY);
            }
            Done::Accounts(Err(state)) => {
                let every = match state {
                    GhState::Failed(_) => poller::AUTH_FAILED,
                    _ => poller::AUTH_RETRY,
                };
                self.auth_beat.finish(now, false, every);
                self.auth = Auth::Failed(state);
                fx.extend(self.snapshot(To::All, projects));
            }
            Done::Discovered { project, mut repos } => {
                self.discovering.remove(&project);
                self.discovered.insert(project);
                repos.sort_by(|a, b| a.path.cmp(&b.path));
                let many = repos.len() > SHOWN_AT_FIRST;
                let mut found = HashSet::new();
                for (i, local) in repos.into_iter().enumerate() {
                    let new = !self
                        .repos
                        .iter()
                        .any(|r| r.stored.project == project && r.stored.path == local.path);
                    let mut stored =
                        match store.upsert_repo(project, &local.path, &local.owner, &local.repo) {
                            Ok(stored) => stored,
                            Err(e) => {
                                tracing::warn!(error = %e, "could not store a GitHub repo");
                                continue;
                            }
                        };
                    // Only a repo seen for the first time: the user's choice stays.
                    if new && many && i >= SHOWN_AT_FIRST {
                        match store.set_repo_visible(stored.id, false) {
                            Ok(()) => stored.visible = false,
                            Err(e) => tracing::warn!(error = %e, "could not hide a GitHub repo"),
                        }
                    }
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
            Done::Undiscovered { project } => {
                self.discovering.remove(&project);
                self.discovered.insert(project);
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
                    r.choose(accounts::pick(&seen));
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
            Done::Inbox {
                project,
                account,
                ids,
                reply,
            } => {
                let ok = reply.is_ok();
                let stamp = rfc3339((now_ms() / 1000) as i64);
                match reply {
                    Ok(reply) => {
                        if let Some(rate) = &reply.rate
                            && rate.remaining < RATE_FLOOR
                        {
                            self.slow_until.insert(account.clone(), now + poller::SLOW);
                        }
                        for (id, result) in ids.iter().zip(reply.repos) {
                            // A repo read by another account since: this answer is stale.
                            let Some(r) = self.repos.iter_mut().find(|r| {
                                r.stored.id == *id && r.account() == Some(account.as_str())
                            }) else {
                                continue;
                            };
                            match result {
                                Ok((mut prs, total)) => {
                                    for p in &mut prs {
                                        let key = (
                                            r.stored.owner.clone(),
                                            r.stored.name.clone(),
                                            p.number,
                                        );
                                        p.unseen = self
                                            .seen
                                            .get(&key)
                                            .is_none_or(|at| at.as_str() < p.updated_at.as_str());
                                    }
                                    let asked: HashSet<u32> = prs
                                        .iter()
                                        .filter(|p| p.requested_you && !p.draft)
                                        .map(|p| p.number)
                                        .collect();
                                    if let Some(before) = &r.asked {
                                        for p in prs.iter().filter(|p| {
                                            asked.contains(&p.number) && !before.contains(&p.number)
                                        }) {
                                            fx.send(
                                                To::All,
                                                ServerEvent::ReviewRequested {
                                                    project,
                                                    pr: PrRef {
                                                        repo: *id,
                                                        number: p.number,
                                                    },
                                                    repo: folder_name(&r.stored.path),
                                                    title: p.title.clone(),
                                                },
                                            );
                                        }
                                    }
                                    r.asked = Some(asked);
                                    r.prs = prs;
                                    r.total = total;
                                    r.viewer = Some(reply.viewer.clone());
                                    r.state = GhState::Ok;
                                    r.fetched_at = Some(stamp.clone());
                                    r.failed_at = None;
                                }
                                Err(state) => {
                                    r.state = state;
                                    r.failed_at = Some(stamp.clone());
                                }
                            }
                        }
                    }
                    Err(state) => {
                        if matches!(state, GhState::RateLimited { .. }) {
                            self.slow_until.insert(account.clone(), now + poller::SLOW);
                        }
                        if state == GhState::LoggedOut && matches!(self.auth, Auth::Ready { .. }) {
                            // A token that stopped working: load the accounts again.
                            self.auth = Auth::Unknown;
                            self.auth_beat.hurry(now);
                        }
                        for r in self.repos.iter_mut().filter(|r| {
                            ids.contains(&r.stored.id) && r.account() == Some(account.as_str())
                        }) {
                            r.state = state.clone();
                            r.failed_at = Some(stamp.clone());
                        }
                    }
                }
                let focused = self.clients.values().any(|f| f.project == Some(project));
                let slow = self.slow_until.get(&account).is_some_and(|t| now < *t);
                if let Some(beat) = self.beats.get_mut(&(project, account)) {
                    beat.finish(now, ok, poller::every(focused, slow));
                }
                fx.send(To::All, self.prs_event(project));
            }
            Done::Detail { pr, reply } => {
                let ok = reply.is_ok();
                if let Some(beat) = self.detail_beats.get_mut(&pr) {
                    beat.finish(now, ok, poller::DETAIL);
                }
                let state = match reply {
                    Ok(detail) => {
                        self.cache(pr, detail);
                        GhState::Ok
                    }
                    Err(state) => state,
                };
                let event = ServerEvent::PrDetail {
                    pr,
                    state,
                    detail: self.cached(pr).cloned().map(Box::new),
                };
                for (client, focus) in &self.clients {
                    if focus.pr == Some(pr) {
                        fx.send(To::One(*client), event.clone());
                    }
                }
            }
            Done::Diff {
                pr,
                head_oid,
                reply,
            } => {
                self.diff_flying.remove(&pr);
                self.diff_stale.remove(&pr);
                let state = match reply {
                    Ok(diff) => {
                        self.diff_failed.remove(&pr);
                        self.cache_diff(pr, diff);
                        GhState::Ok
                    }
                    Err(state) => {
                        self.diff_failed.insert(pr, head_oid);
                        state
                    }
                };
                let event = ServerEvent::PrDiff {
                    pr,
                    state,
                    diff: self.cached_diff(pr).cloned().map(Box::new),
                };
                self.to_diff_watchers(pr, event, &mut fx);
            }
            Done::Written {
                pr,
                client,
                ticket,
                what,
                reply,
            } => {
                self.writing.remove(&pr);
                match reply {
                    Ok(()) => {
                        fx.send(To::One(client), ServerEvent::PrWritten { pr, ticket });
                        // Show what was written as GitHub has it now.
                        self.detail_beats.entry(pr).or_default().hurry(now);
                    }
                    Err(state) => fx.send(
                        To::One(client),
                        ServerEvent::PrWriteFailed {
                            pr,
                            ticket: Some(ticket),
                            message: format!("couldn't {what} · {}", said(&state)),
                        },
                    ),
                }
                self.next_write(pr, &mut fx);
            }
            Done::Marked {
                pr,
                path,
                viewed,
                client,
                reply,
            } => match reply {
                Ok(()) => {
                    let now = if viewed {
                        Viewed::Viewed
                    } else {
                        Viewed::Unviewed
                    };
                    for (_, d) in self.diffs.iter_mut().filter(|(p, _)| *p == pr) {
                        for f in d.files.iter_mut().filter(|f| f.path == path) {
                            f.viewed = now;
                        }
                    }
                    for (_, d) in self.details.iter_mut().filter(|(p, _)| *p == pr) {
                        for f in d.files.iter_mut().filter(|f| f.path == path) {
                            f.viewed = now;
                        }
                    }
                    if let Some(d) = self.cached_diff(pr) {
                        let event = ServerEvent::PrDiff {
                            pr,
                            state: GhState::Ok,
                            diff: Some(Box::new(d.clone())),
                        };
                        self.to_diff_watchers(pr, event, &mut fx);
                    }
                    if let Some(d) = self.cached(pr) {
                        let event = ServerEvent::PrDetail {
                            pr,
                            state: GhState::Ok,
                            detail: Some(Box::new(d.clone())),
                        };
                        for (c, focus) in &self.clients {
                            if focus.pr == Some(pr) {
                                fx.send(To::One(*c), event.clone());
                            }
                        }
                    }
                }
                Err(state) => fx.send(
                    To::One(client),
                    ServerEvent::PrWriteFailed {
                        pr,
                        ticket: None,
                        message: format!(
                            "couldn't mark {} {} · {}",
                            file_name(&path),
                            if viewed { "viewed" } else { "unviewed" },
                            said(&state)
                        ),
                    },
                ),
            },
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

        /// Another client connects and turns GitHub on, as the TUI does.
        fn join(&mut self, client: ClientId) {
            self.gh.request(
                client,
                ClientRequest::SetGitHub { enabled: true },
                &self.store,
                &self.projects,
                self.now,
            );
        }

        fn done(&mut self, done: Done) -> Effects {
            self.gh.done(done, self.now, &self.store, &self.projects)
        }

        /// A client connected, GitHub on, these accounts loaded.
        fn ready(&mut self, accounts: Vec<Account>) {
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
        w.request(ClientRequest::SetGitHub { enabled: false });
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
    fn accounts_out_of_reach_are_tried_again_soon() {
        let mut w = world(&["work"]);
        w.request(ClientRequest::SetGitHub { enabled: true });
        w.tick();
        let fx = w.done(Done::Accounts(Err(GhState::Failed(
            "could not reach GitHub".into(),
        ))));
        assert_eq!(
            shown(&fx).map(|s| s.0),
            Some(GhState::Failed("could not reach GitHub".into()))
        );
        w.now += 59 * S;
        assert!(w.tick().jobs.is_empty());
        w.now += S;
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
    fn a_folder_of_many_repos_shows_its_first_ten() {
        let mut w = world(&["code"]);
        w.ready(vec![account("work", true)]);
        let names: Vec<String> = (0..12).map(|i| format!("r{i:02}")).collect();
        // Out of order: the first ten are taken by path.
        let found: Vec<(&str, &str, &str)> = names
            .iter()
            .rev()
            .map(|n| (n.as_str(), "acme", n.as_str()))
            .collect();
        let fx = w.found(0, &found);
        let visible: Vec<bool> = listed(&fx).iter().map(|r| r.2).collect();
        let mut want = vec![true; 12];
        want[10] = false;
        want[11] = false;
        assert_eq!(visible, want);
        assert_eq!(shown(&fx).unwrap().1, 12, "all twelve are found");
        assert_eq!(
            w.store
                .repos()
                .unwrap()
                .iter()
                .filter(|r| r.visible)
                .count(),
            10
        );
        // The user's choice stays when the folder is looked through again.
        w.request(ClientRequest::SetRepoVisible {
            repo: w.id("r11"),
            visible: true,
        });
        w.request(ClientRequest::SetRepoVisible {
            repo: w.id("r00"),
            visible: false,
        });
        let fx = w.found(0, &found);
        let visible: Vec<bool> = listed(&fx).iter().map(|r| r.2).collect();
        let mut want = vec![true; 12];
        want[0] = false;
        want[10] = false;
        assert_eq!(visible, want);
    }

    #[test]
    fn a_look_through_that_failed_keeps_what_was_stored() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true)]);
        w.found(0, &[("site", "acme", "site")]);
        let project = w.projects[0].id;
        w.request(ClientRequest::ListRepos { project });
        let fx = w.done(Done::Undiscovered { project });
        assert_eq!(listed(&fx), [("site".to_string(), None, true)]);
        assert!(
            !w.tick()
                .jobs
                .iter()
                .any(|j| matches!(j, Job::Discover { .. })),
            "not looked through again at once"
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

    use std::time::Duration;

    const S: Duration = Duration::from_secs(1);

    fn pr(number: u32, requested_you: bool, updated_at: &str) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/site/pull/{number}"),
            author: "bob".into(),
            draft: false,
            state: termist_core::github::PrState::Open,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: updated_at.into(),
            head: "feat".into(),
            base: "main".into(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
            mergeable: termist_core::github::Mergeable::Yes,
            decision: None,
            requested: if requested_you {
                vec!["alice".into()]
            } else {
                vec![]
            },
            requested_you,
            verdicts: vec![],
            checks: termist_core::github::Checks::Passing,
            unseen: false,
        }
    }

    fn reply(
        remaining: u32,
        repos: Vec<Result<(Vec<PrSummary>, u32), GhState>>,
    ) -> query::InboxReply {
        query::InboxReply {
            viewer: "alice".into(),
            rate: Some(query::Rate {
                remaining,
                reset_at: "2026-10-02T11:00:00Z".into(),
            }),
            repos,
        }
    }

    /// Inbox jobs of `fx`: (project index, account, repo names).
    fn rounds(w: &World, fx: &Effects) -> Vec<(usize, String, Vec<String>)> {
        fx.jobs
            .iter()
            .filter_map(|j| match j {
                Job::Inbox {
                    project,
                    account,
                    repos,
                    ..
                } => Some((
                    w.projects.iter().position(|p| p.id == *project).unwrap(),
                    account.login.clone(),
                    repos.iter().map(|(_, _, n)| n.clone()).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    /// The inbox jobs the next tick starts.
    fn ticked(w: &mut World) -> Vec<(usize, String, Vec<String>)> {
        let fx = w.tick();
        rounds(w, &fx)
    }

    /// A work project with acme/site and acme/admin, a personal one with me/termist,
    /// each repo read by its account, nothing read yet.
    fn two_projects() -> World {
        let mut w = world(&["work", "termist"]);
        w.ready(vec![account("work", true), account("me", false)]);
        w.found(0, &[("site", "acme", "site"), ("admin", "acme", "admin")]);
        w.found(1, &[("termist", "me", "termist")]);
        w.permit(&[
            ("site", &[("work", Some(Write), true)]),
            ("admin", &[("work", Some(Write), true)]),
            (
                "termist",
                &[("work", Some(Read), true), ("me", Some(Admin), false)],
            ),
        ]);
        w
    }

    /// Answers project `i`'s round for `account` with `prs` in its first repo.
    fn answer(
        w: &mut World,
        i: usize,
        account: &str,
        names: &[&str],
        first: Vec<PrSummary>,
    ) -> Effects {
        let ids: Vec<RepoId> = names.iter().map(|n| w.id(n)).collect();
        let mut repos = vec![Ok((first.clone(), first.len() as u32))];
        repos.extend((1..ids.len()).map(|_| Ok((vec![], 0))));
        let project = w.projects[i].id;
        w.done(Done::Inbox {
            project,
            account: account.into(),
            ids,
            reply: Ok(reply(4000, repos)),
        })
    }

    #[test]
    fn an_inbox_round_per_project_and_account() {
        let mut w = two_projects();
        w.request(ClientRequest::SetRepoVisible {
            repo: w.id("admin"),
            visible: false,
        });
        let fx = w.tick();
        assert_eq!(
            rounds(&w, &fx),
            [
                (0, "work".to_string(), vec!["site".to_string()]),
                (1, "me".to_string(), vec!["termist".to_string()]),
            ]
        );
        assert!(ticked(&mut w).is_empty(), "not twice");
    }

    #[test]
    fn a_focused_project_is_read_every_30s_others_every_3m() {
        let mut w = two_projects();
        let p0 = w.projects[0].id;
        w.request(ClientRequest::SetPrFocus {
            project: Some(p0),
            pr: None,
            diff: false,
        });
        w.tick();
        answer(&mut w, 0, "work", &["site", "admin"], vec![]);
        answer(&mut w, 1, "me", &["termist"], vec![]);
        w.now += 30 * S;
        assert_eq!(ticked(&mut w).iter().map(|r| r.0).collect::<Vec<_>>(), [0]);
        answer(&mut w, 0, "work", &["site", "admin"], vec![]);
        w.now += 150 * S;
        let due: Vec<usize> = ticked(&mut w).iter().map(|r| r.0).collect();
        assert_eq!(due, [0, 1]);
    }

    #[test]
    fn a_failed_round_keeps_the_last_list() {
        let mut w = two_projects();
        w.tick();
        answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, false, "2026-10-02T10:00:00Z")],
        );
        let project = w.projects[0].id;
        let fx = w.done(Done::Inbox {
            project,
            account: "work".into(),
            ids: vec![w.id("site"), w.id("admin")],
            reply: Err(GhState::Failed("HTTP 502".into())),
        });
        let Some((_, ServerEvent::Prs { repos, .. })) = fx.events.last() else {
            panic!()
        };
        let site = repos.iter().find(|r| r.name == "site").unwrap();
        assert_eq!(site.state, GhState::Failed("HTTP 502".into()));
        assert_eq!(site.prs.len(), 1, "the last list stays");
        assert!(site.fetched_at.is_some() && site.failed_at.is_some());
    }

    #[test]
    fn a_review_request_after_the_first_round_is_announced_once() {
        let mut w = two_projects();
        w.tick();
        let announced = |fx: &Effects| -> Vec<u32> {
            fx.events
                .iter()
                .filter_map(|(_, e)| match e {
                    ServerEvent::ReviewRequested { pr, .. } => Some(pr.number),
                    _ => None,
                })
                .collect()
        };
        let t = "2026-10-02T10:00:00Z";
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, true, t)],
        );
        assert!(announced(&fx).is_empty(), "the first round only learns");
        let mut draft = pr(216, true, t);
        draft.draft = true;
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, true, t), pr(215, true, t), draft],
        );
        assert_eq!(announced(&fx), [215]);
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, true, t), pr(215, true, t)],
        );
        assert!(announced(&fx).is_empty());
    }

    #[test]
    fn unseen_follows_what_you_opened() {
        let mut w = two_projects();
        w.tick();
        let unseen = |fx: &Effects| -> Vec<bool> {
            match fx.events.last() {
                Some((_, ServerEvent::Prs { repos, .. })) => repos
                    .iter()
                    .find(|r| r.name == "site")
                    .unwrap()
                    .prs
                    .iter()
                    .map(|p| p.unseen)
                    .collect(),
                _ => panic!(),
            }
        };
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, false, "2026-10-02T10:00:00Z")],
        );
        assert_eq!(unseen(&fx), [true]);
        let site = w.id("site");
        let fx = w.request(ClientRequest::MarkPrSeen {
            pr: PrRef {
                repo: site,
                number: 212,
            },
            updated_at: "2026-10-02T10:00:00Z".into(),
        });
        assert_eq!(unseen(&fx), [false]);
        assert_eq!(w.store.seen().unwrap().len(), 1);
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, false, "2026-10-02T10:00:00Z")],
        );
        assert_eq!(unseen(&fx), [false]);
        let fx = answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, false, "2026-10-02T11:00:00Z")],
        );
        assert_eq!(unseen(&fx), [true], "changed since");
    }

    #[test]
    fn the_open_pr_is_read_and_sent_to_who_looks() {
        let mut w = two_projects();
        let other = ClientId(2);
        w.join(other);
        let pr = PrRef {
            repo: w.id("site"),
            number: 212,
        };
        w.request(ClientRequest::SetPrFocus {
            project: Some(w.projects[0].id),
            pr: Some(pr),
            diff: false,
        });
        let fx = w.tick();
        assert!(
            fx.jobs
                .iter()
                .any(|j| matches!(j, Job::Detail { pr: p, .. } if *p == pr))
        );
        let detail = PrDetail {
            summary: super::query::summary(
                &serde_json::json!({"number": 212, "title": "x"}),
                "alice",
            )
            .unwrap(),
            id: "PR_1".into(),
            head_oid: "h1".into(),
            mine: false,
            pending_review: None,
            body: "body".into(),
            comments: vec![],
            reviews: vec![],
            threads: vec![],
            checks: vec![],
            files: vec![],
            more: Default::default(),
        };
        let fx = w.done(Done::Detail {
            pr,
            reply: Ok(detail),
        });
        assert_eq!(fx.events.len(), 1);
        assert_eq!(fx.events[0].0, To::One(w.client));
        let fx = w.gh.request(
            other,
            ClientRequest::SetPrFocus {
                project: None,
                pr: Some(pr),
                diff: false,
            },
            &w.store,
            &w.projects,
            w.now,
        );
        assert!(
            matches!(&fx.events[0], (To::One(c), ServerEvent::PrDetail { detail: Some(_), .. }) if *c == other),
            "the second one gets it at once"
        );
    }

    #[test]
    fn a_client_that_leaves_drops_its_focus() {
        let mut w = two_projects();
        let pr = PrRef {
            repo: w.id("site"),
            number: 212,
        };
        w.request(ClientRequest::SetPrFocus {
            project: Some(w.projects[0].id),
            pr: Some(pr),
            diff: false,
        });
        w.join(ClientId(2));
        w.gh.gone(w.client);
        let fx = w.tick();
        assert!(!fx.jobs.iter().any(|j| matches!(j, Job::Detail { .. })));
    }

    #[test]
    fn a_late_focus_does_not_bring_a_gone_client_back() {
        let mut w = two_projects();
        w.gh.gone(w.client);
        w.request(ClientRequest::SetPrFocus {
            project: Some(w.projects[0].id),
            pr: None,
            diff: false,
        });
        assert!(w.tick().jobs.is_empty());
    }

    #[test]
    fn a_client_that_never_turned_github_on_reads_nothing() {
        let mut w = two_projects();
        w.gh.gone(w.client);
        // A hook connection: it asks nothing of GitHub, or only for a focus.
        let hook = ClientId(2);
        w.gh.request(
            hook,
            ClientRequest::SetPrFocus {
                project: Some(w.projects[0].id),
                pr: None,
                diff: false,
            },
            &w.store,
            &w.projects,
            w.now,
        );
        assert!(w.tick().jobs.is_empty());
        w.join(hook);
        assert!(!w.tick().jobs.is_empty(), "once it turns GitHub on");
    }

    #[test]
    fn nothing_is_read_without_a_client() {
        let mut w = two_projects();
        w.gh.gone(w.client);
        assert!(w.tick().jobs.is_empty());
    }

    #[test]
    fn a_low_rate_limit_slows_that_account() {
        let mut w = two_projects();
        w.request(ClientRequest::SetPrFocus {
            project: Some(w.projects[0].id),
            pr: None,
            diff: false,
        });
        w.tick();
        let ids = vec![w.id("site"), w.id("admin")];
        let project = w.projects[0].id;
        w.done(Done::Inbox {
            project,
            account: "work".into(),
            ids,
            reply: Ok(reply(100, vec![Ok((vec![], 0)), Ok((vec![], 0))])),
        });
        w.now += 60 * S;
        assert!(ticked(&mut w).iter().all(|r| r.1 != "work"));
        w.now += 540 * S;
        assert!(ticked(&mut w).iter().any(|r| r.1 == "work"));
    }

    #[test]
    fn a_token_that_stops_working_loads_the_accounts_again() {
        let mut w = two_projects();
        w.tick();
        let project = w.projects[1].id;
        let ids = vec![w.id("termist")];
        w.done(Done::Inbox {
            project,
            account: "me".into(),
            ids,
            reply: Err(GhState::LoggedOut),
        });
        assert!(matches!(w.tick().jobs.first(), Some(Job::Accounts)));
    }

    #[test]
    fn refresh_reads_now_and_retries_failed_accounts() {
        let mut w = two_projects();
        w.tick();
        answer(&mut w, 0, "work", &["site", "admin"], vec![]);
        let project = w.projects[0].id;
        w.request(ClientRequest::RefreshPrs { project });
        assert_eq!(ticked(&mut w).len(), 1);

        let mut w = world(&["work"]);
        w.request(ClientRequest::SetGitHub { enabled: true });
        w.tick();
        w.done(Done::Accounts(Err(GhState::LoggedOut)));
        let project = w.projects[0].id;
        w.request(ClientRequest::RefreshPrs { project });
        assert!(matches!(w.tick().jobs[..], [Job::Accounts]));
    }

    /// The accounts jobs among `fx`.
    fn reloads(fx: &Effects) -> usize {
        fx.jobs
            .iter()
            .filter(|j| matches!(j, Job::Accounts))
            .count()
    }

    #[test]
    fn refresh_and_the_repos_window_load_the_accounts_again_and_keep_reading() {
        let mut w = two_projects();
        let project = w.projects[0].id;
        w.request(ClientRequest::RefreshPrs { project });
        let fx = w.tick();
        assert_eq!(reloads(&fx), 1);
        assert!(!rounds(&w, &fx).is_empty(), "the old accounts still read");
        assert_eq!(reloads(&w.tick()), 0, "one at a time");
        // A reload that fails keeps what was read.
        let fx = w.done(Done::Accounts(Err(GhState::Failed("timeout".into()))));
        assert!(fx.events.is_empty());
        assert_eq!(w.gh.repo(w.id("site")).unwrap().account(), Some("work"));
        w.request(ClientRequest::ListRepos { project });
        assert_eq!(reloads(&w.tick()), 1);
        w.done(Done::Accounts(Ok((
            handle(),
            vec![account("work", true), account("me", false)],
        ))));
        assert_eq!(
            w.gh.repo(w.id("site")).unwrap().account(),
            Some("work"),
            "still read by the same account until access is asked again"
        );
    }

    #[test]
    fn a_repo_no_account_could_see_is_asked_again_on_refresh() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true)]);
        w.found(0, &[("site", "acme", "site")]);
        w.permit(&[("site", &[("work", None, true)])]);
        assert_eq!(w.gh.repo(w.id("site")).unwrap().state, GhState::NoAccess);
        assert!(
            !w.tick()
                .jobs
                .iter()
                .any(|j| matches!(j, Job::Permissions { .. })),
            "asked once"
        );
        let project = w.projects[0].id;
        w.request(ClientRequest::RefreshPrs { project });
        w.permit(&[("site", &[("work", Some(Read), true)])]);
        assert_eq!(w.gh.repo(w.id("site")).unwrap().account(), Some("work"));
    }

    #[test]
    fn another_reader_announces_nothing_on_its_first_round() {
        let mut w = world(&["work"]);
        w.ready(vec![account("work", true), account("me", false)]);
        w.found(0, &[("site", "acme", "site")]);
        w.permit(&[("site", &[("work", Some(Write), true), ("me", None, false)])]);
        w.tick();
        let t = "2026-10-02T10:00:00Z";
        answer(&mut w, 0, "work", &["site"], vec![pr(212, true, t)]);
        assert!(w.gh.repo(w.id("site")).unwrap().asked.is_some());
        // Access changed: the other account reads it now.
        let project = w.projects[0].id;
        w.request(ClientRequest::RefreshPrs { project });
        assert_eq!(reloads(&w.tick()), 1);
        w.done(Done::Accounts(Ok((
            handle(),
            vec![account("work", true), account("me", false)],
        ))));
        w.permit(&[("site", &[("work", None, true), ("me", Some(Admin), false)])]);
        let r = w.gh.repo(w.id("site")).unwrap();
        assert_eq!(r.account(), Some("me"));
        assert!(r.asked.is_none(), "a new viewer learns first");
    }

    #[test]
    fn an_answer_for_an_account_that_no_longer_reads_the_repo_is_dropped() {
        let mut w = two_projects();
        w.tick();
        let site = w.id("site");
        w.request(ClientRequest::SetRepoAccount {
            repo: site,
            account: Some("me".into()),
        });
        let t = "2026-10-02T10:00:00Z";
        answer(
            &mut w,
            0,
            "work",
            &["site", "admin"],
            vec![pr(212, true, t)],
        );
        let r = w.gh.repo(site).unwrap();
        assert!(r.prs.is_empty() && r.viewer.is_none() && r.asked.is_none());
        assert_eq!(
            w.gh.repo(w.id("admin")).unwrap().viewer.as_deref(),
            Some("alice"),
            "its other repos still count"
        );
        let project = w.projects[0].id;
        w.done(Done::Inbox {
            project,
            account: "work".into(),
            ids: vec![site],
            reply: Err(GhState::Failed("HTTP 502".into())),
        });
        assert_eq!(w.gh.repo(site).unwrap().state, GhState::Ok);
    }

    #[test]
    fn loaded_accounts_clear_a_failure_every_client_still_shows() {
        let mut w = world(&["work"]);
        w.request(ClientRequest::SetGitHub { enabled: true });
        w.tick();
        w.found(0, &[]);
        let fx = w.done(Done::Accounts(Err(GhState::NoGh)));
        assert_eq!(shown(&fx).map(|s| s.0), Some(GhState::NoGh));
        let fx = w.done(Done::Accounts(Ok((handle(), vec![account("work", true)]))));
        assert!(
            matches!(
                fx.events.last(),
                Some((
                    To::All,
                    ServerEvent::Prs {
                        state: GhState::Ok,
                        ..
                    }
                ))
            ),
            "{:?}",
            fx.events
        );
    }

    /// Pull request 212 of acme/site, at head `head`, with one file.
    fn detail_at(head: &str) -> PrDetail {
        PrDetail {
            summary: super::query::summary(
                &serde_json::json!({"number": 212, "title": "x",
                    "url": "https://github.com/acme/site/pull/212", "changedFiles": 1}),
                "alice",
            )
            .unwrap(),
            id: "PR_212".into(),
            head_oid: head.into(),
            mine: false,
            pending_review: None,
            body: String::new(),
            comments: vec![],
            reviews: vec![],
            threads: vec![],
            checks: vec![],
            files: vec![termist_core::github::FileChange {
                path: "src/a.rs".into(),
                additions: 1,
                deletions: 0,
                change: 'M',
                viewed: Viewed::Unviewed,
            }],
            more: Default::default(),
        }
    }

    fn diff_at(head: &str) -> PrDiff {
        PrDiff {
            head_oid: head.into(),
            files: vec![termist_core::github::DiffFile {
                path: "src/a.rs".into(),
                previous: None,
                change: 'M',
                additions: 1,
                deletions: 0,
                viewed: Viewed::Unviewed,
                patch: termist_core::github::Patch::Text("@@ -1 +1 @@\n-a\n+b".into()),
                url: String::new(),
            }],
            more: 0,
        }
    }

    /// The heads of the diff jobs among `fx`.
    fn diff_jobs(fx: &Effects) -> Vec<String> {
        fx.jobs
            .iter()
            .filter_map(|j| match j {
                Job::Diff { want, .. } => Some(want.head_oid.clone()),
                _ => None,
            })
            .collect()
    }

    /// Two projects, pull request 212 of site looked at, its diff too if `diff`.
    fn looking(diff: bool) -> (World, PrRef) {
        let mut w = two_projects();
        let pr = PrRef {
            repo: w.id("site"),
            number: 212,
        };
        w.request(ClientRequest::SetPrFocus {
            project: Some(w.projects[0].id),
            pr: Some(pr),
            diff,
        });
        (w, pr)
    }

    #[test]
    fn the_diff_is_read_once_its_head_is_known() {
        let (mut w, pr) = looking(true);
        let fx = w.tick();
        assert!(diff_jobs(&fx).is_empty(), "the head comes with the detail");
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        let fx = w.tick();
        assert_eq!(diff_jobs(&fx), ["h1"]);
        let Some(Job::Diff { want, account, .. }) =
            fx.jobs.iter().find(|j| matches!(j, Job::Diff { .. }))
        else {
            panic!()
        };
        assert_eq!(
            (want.owner.as_str(), want.name.as_str(), want.changed),
            ("acme", "site", 1)
        );
        assert_eq!(account.login, "work");
        assert!(diff_jobs(&w.tick()).is_empty(), "one at a time");
        let fx = w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        assert!(matches!(
            &fx.events[..],
            [(To::One(c), ServerEvent::PrDiff { state: GhState::Ok, diff: Some(_), .. })] if *c == w.client
        ));
        w.now += Duration::from_secs(25);
        assert!(diff_jobs(&w.tick()).is_empty(), "read at this head");
    }

    #[test]
    fn a_new_head_reads_the_diff_again() {
        let (mut w, pr) = looking(true);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.tick();
        w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h2")),
        });
        assert_eq!(diff_jobs(&w.tick()), ["h2"]);
    }

    #[test]
    fn a_failed_diff_keeps_the_last_one_and_waits_for_a_refresh() {
        let (mut w, pr) = looking(true);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.tick();
        w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h2")),
        });
        w.tick();
        let fx = w.done(Done::Diff {
            pr,
            head_oid: "h2".into(),
            reply: Err(GhState::Failed("HTTP 502".into())),
        });
        assert!(matches!(
            &fx.events[..],
            [(_, ServerEvent::PrDiff { state: GhState::Failed(_), diff: Some(d), .. })] if d.head_oid == "h1"
        ));
        assert!(diff_jobs(&w.tick()).is_empty(), "not again at this head");
        w.request(ClientRequest::RefreshPrs {
            project: w.projects[0].id,
        });
        assert_eq!(diff_jobs(&w.tick()), ["h2"]);
    }

    #[test]
    fn a_refresh_that_fails_keeps_the_diff_on_screen() {
        let (mut w, pr) = looking(true);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.tick();
        w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        w.request(ClientRequest::RefreshPrs {
            project: w.projects[0].id,
        });
        assert_eq!(diff_jobs(&w.tick()), ["h1"], "read again at the same head");
        let fx = w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Err(GhState::Failed("HTTP 502".into())),
        });
        assert!(matches!(
            &fx.events[..],
            [(_, ServerEvent::PrDiff { state: GhState::Failed(_), diff: Some(d), .. })] if d.head_oid == "h1"
        ));
        assert!(diff_jobs(&w.tick()).is_empty(), "not again until asked");
        let other = ClientId(2);
        w.join(other);
        let fx = w.gh.request(
            other,
            ClientRequest::SetPrFocus {
                project: None,
                pr: Some(pr),
                diff: true,
            },
            &w.store,
            &w.projects,
            w.now,
        );
        assert!(
            fx.events
                .iter()
                .any(|(_, e)| matches!(e, ServerEvent::PrDiff { diff: Some(_), .. })),
            "the diff read before is still there for a newcomer"
        );
    }

    #[test]
    fn the_detail_alone_reads_no_diff() {
        let (mut w, pr) = looking(false);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        assert!(diff_jobs(&w.tick()).is_empty());
    }

    #[test]
    fn a_client_that_comes_to_the_diff_gets_the_one_read() {
        let (mut w, pr) = looking(true);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.tick();
        w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        let other = ClientId(2);
        w.join(other);
        let fx = w.gh.request(
            other,
            ClientRequest::SetPrFocus {
                project: None,
                pr: Some(pr),
                diff: true,
            },
            &w.store,
            &w.projects,
            w.now,
        );
        assert!(
            fx.events
                .iter()
                .any(|(to, e)| *to == To::One(other) && matches!(e, ServerEvent::PrDiff { .. }))
        );
    }

    #[test]
    fn marking_a_file_viewed_writes_through_the_reader_and_sends_both_views() {
        let (mut w, pr) = looking(true);
        let ask = ClientRequest::SetFileViewed {
            pr,
            path: "src/a.rs".into(),
            viewed: true,
        };
        let fx = w.request(ask.clone());
        assert!(matches!(
            &fx.events[..],
            [(To::One(_), ServerEvent::PrWriteFailed { message, .. })] if message == "couldn't mark a.rs viewed · not loaded yet"
        ));
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.tick();
        w.done(Done::Diff {
            pr,
            head_oid: "h1".into(),
            reply: Ok(diff_at("h1")),
        });
        let fx = w.request(ask);
        let Some(Job::MarkViewed {
            id,
            account,
            client,
            ..
        }) = fx.jobs.first()
        else {
            panic!("{:?}", fx.jobs)
        };
        assert_eq!(
            (id.as_str(), account.login.as_str(), *client),
            ("PR_212", "work", w.client)
        );
        let fx = w.done(Done::Marked {
            pr,
            path: "src/a.rs".into(),
            viewed: true,
            client: w.client,
            reply: Ok(()),
        });
        let diff = fx.events.iter().find_map(|(_, e)| match e {
            ServerEvent::PrDiff { diff: Some(d), .. } => Some(d.files[0].viewed),
            _ => None,
        });
        let detail = fx.events.iter().find_map(|(_, e)| match e {
            ServerEvent::PrDetail {
                detail: Some(d), ..
            } => Some(d.files[0].viewed),
            _ => None,
        });
        assert_eq!((diff, detail), (Some(Viewed::Viewed), Some(Viewed::Viewed)));
    }

    #[test]
    fn a_refused_mark_is_told_to_the_client_that_asked() {
        let (mut w, pr) = looking(true);
        w.join(ClientId(2));
        let fx = w.done(Done::Marked {
            pr,
            path: "src/search/DealerFilter.tsx".into(),
            viewed: false,
            client: w.client,
            reply: Err(GhState::Failed(
                "Resource not accessible by integration".into(),
            )),
        });
        assert_eq!(
            fx.events,
            [(
                To::One(w.client),
                ServerEvent::PrWriteFailed {
                    pr,
                    ticket: None,
                    message: "couldn't mark DealerFilter.tsx unviewed · Resource not accessible by integration".into(),
                }
            )]
        );
    }

    /// A comment on `pr` asked under `ticket`.
    fn comment_on(pr: PrRef, ticket: u64) -> ClientRequest {
        ClientRequest::WritePr {
            pr,
            ticket,
            write: PrWrite::Comment { body: "hi".into() },
        }
    }

    fn write_jobs(fx: &Effects) -> Vec<(u64, String, String)> {
        fx.jobs
            .iter()
            .filter_map(|j| match j {
                Job::Write {
                    ticket,
                    account,
                    at,
                    ..
                } => Some((
                    *ticket,
                    account.login.clone(),
                    format!("{}/{}#{} {}", at.owner, at.name, at.number, at.id),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_write_runs_as_the_reader_one_at_a_time_and_answers_its_ticket() {
        let (mut w, pr) = looking(false);
        let fx = w.request(comment_on(pr, 1));
        assert_eq!(
            fx.events,
            [(
                To::One(w.client),
                ServerEvent::PrWriteFailed {
                    pr,
                    ticket: Some(1),
                    message: "couldn't post your comment · not loaded yet".into(),
                }
            )]
        );
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        let fx = w.request(comment_on(pr, 2));
        assert_eq!(
            write_jobs(&fx),
            [(2, "work".to_string(), "acme/site#212 PR_212".to_string())]
        );
        assert!(
            write_jobs(&w.request(comment_on(pr, 3))).is_empty(),
            "one at a time"
        );
        let fx = w.done(Done::Written {
            pr,
            client: w.client,
            ticket: 2,
            what: "post your comment",
            reply: Ok(()),
        });
        assert!(
            fx.events
                .contains(&(To::One(w.client), ServerEvent::PrWritten { pr, ticket: 2 }))
        );
        assert_eq!(write_jobs(&fx).len(), 1, "then the next");
        assert!(
            w.tick()
                .jobs
                .iter()
                .any(|j| matches!(j, Job::Detail { pr: p, .. } if *p == pr)),
            "what was written is read back at once"
        );
    }

    #[test]
    fn a_refused_write_tells_only_the_writer() {
        let (mut w, pr) = looking(false);
        w.done(Done::Detail {
            pr,
            reply: Ok(detail_at("h1")),
        });
        w.join(ClientId(2));
        w.request(comment_on(pr, 4));
        let fx = w.done(Done::Written {
            pr,
            client: w.client,
            ticket: 4,
            what: "send your review",
            reply: Err(GhState::Failed(
                "Review Can not approve your own pull request".into(),
            )),
        });
        assert_eq!(
            fx.events,
            [(
                To::One(w.client),
                ServerEvent::PrWriteFailed {
                    pr,
                    ticket: Some(4),
                    message:
                        "couldn't send your review · Review Can not approve your own pull request"
                            .into(),
                }
            )]
        );
    }
}
