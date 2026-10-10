//! Defter: a project's open issues. They are read only while a client looks at them
//! (the Issues tab): nothing is announced from them, so they spend no hourly budget
//! in the background.
use super::accounts::Account;
use super::gh::GhHandle;
use super::poller;
use super::query::IssuesReply;
use super::{Auth, Effects, GitHub, Job, RATE_FLOOR, To, folder_name};
use crate::session::ClientId;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use termist_core::github::{GhState, IssueSummary, RepoId, RepoIssues, rfc3339};
use termist_core::status::now_ms;
use termist_core::{ProjectId, ServerEvent};

/// How often the issues someone looks at are read again.
pub const EVERY: Duration = Duration::from_secs(60);

/// What was last read of one repo's issues.
#[derive(Clone, Debug)]
pub struct Read {
    state: GhState,
    viewer: Option<String>,
    enabled: bool,
    issues: Vec<IssueSummary>,
    total: u32,
    fetched_at: Option<String>,
    failed_at: Option<String>,
}

impl Default for Read {
    fn default() -> Read {
        Read {
            state: GhState::Ok,
            viewer: None,
            enabled: true,
            issues: vec![],
            total: 0,
            fetched_at: None,
            failed_at: None,
        }
    }
}

impl GitHub {
    /// The clients looking at `project`'s issues.
    fn issue_watchers(&self, project: ProjectId) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter(|(_, f)| f.issues && f.project == Some(project))
            .map(|(c, _)| *c)
            .collect()
    }

    fn issues_event(&self, project: ProjectId) -> ServerEvent {
        let state = match &self.auth {
            Auth::Failed(state) => state.clone(),
            _ => GhState::Ok,
        };
        let unread = Read::default();
        ServerEvent::Issues {
            project,
            state,
            repos: self
                .present(project)
                .iter()
                .filter(|r| r.stored.visible)
                .map(|r| {
                    let read = self.issues.get(&r.stored.id).unwrap_or(&unread);
                    RepoIssues {
                        repo: r.stored.id,
                        name: folder_name(&r.stored.path),
                        slug: format!("{}/{}", r.stored.owner, r.stored.name),
                        // No account reads it: the pull requests say why, and so do these.
                        state: if r.account().is_none() {
                            r.state.clone()
                        } else {
                            read.state.clone()
                        },
                        viewer: read.viewer.clone(),
                        enabled: read.enabled,
                        issues: read.issues.clone(),
                        total: read.total,
                        fetched_at: read.fetched_at.clone(),
                        failed_at: read.failed_at.clone(),
                    }
                })
                .collect(),
        }
    }

    /// `project`'s issues to the clients looking at them; with `only`, to that one if
    /// it looks.
    pub(super) fn send_issues(&self, project: ProjectId, only: To, fx: &mut Effects) {
        for client in self.issue_watchers(project) {
            if only == To::All || only == To::One(client) {
                fx.send(To::One(client), self.issues_event(project));
            }
        }
    }

    /// `client` now looks where its focus says; `before` is the project whose issues it
    /// looked at until now. One that starts looking gets what was read at once, and a
    /// read soon if that is old.
    pub(super) fn issues_looked_at(
        &mut self,
        client: ClientId,
        before: Option<ProjectId>,
        now: Instant,
        fx: &mut Effects,
    ) {
        let Some(project) = self
            .clients
            .get(&client)
            .and_then(|f| f.project.filter(|_| f.issues))
        else {
            return;
        };
        if before == Some(project) {
            return;
        }
        // Before the project is looked through, "no repo" would be a guess.
        if self.discovered.contains(&project) || matches!(self.auth, Auth::Failed(_)) {
            fx.send(To::One(client), self.issues_event(project));
        }
        for ((p, _), beat) in &mut self.issue_beats {
            if *p == project {
                beat.freshen(now, EVERY);
            }
        }
    }

    /// `R`: the issues looked at are read now.
    pub(super) fn hurry_issues(&mut self, project: ProjectId, now: Instant) {
        for ((p, _), beat) in &mut self.issue_beats {
            if *p == project {
                beat.hurry(now);
            }
        }
    }

    /// The reads due: one per project looked at and account.
    pub(super) fn issue_rounds(
        &mut self,
        now: Instant,
        gh: &GhHandle,
        accounts: &[Account],
    ) -> Vec<Job> {
        let looked: HashSet<ProjectId> = self
            .clients
            .values()
            .filter(|f| f.issues)
            .filter_map(|f| f.project)
            .collect();
        // Nobody looks: forgotten, so the next look reads at once.
        self.issue_beats
            .retain(|(p, _), beat| looked.contains(p) || beat.in_flight());
        let mut jobs = vec![];
        for project in looked {
            for (account, repos) in self.batches(project, accounts, |r| r.stored.visible) {
                let beat = self
                    .issue_beats
                    .entry((project, account.login.clone()))
                    .or_default();
                if beat.due(now) {
                    beat.start();
                    jobs.push(Job::Issues {
                        gh: gh.clone(),
                        project,
                        account,
                        repos,
                    });
                }
            }
        }
        jobs
    }

    /// Repo `id` is still read by `account`: an answer of another reader is stale.
    fn reads_with(&self, id: RepoId, account: &str) -> bool {
        self.repo(id).is_some_and(|r| r.account() == Some(account))
    }

    pub(super) fn issues_done(
        &mut self,
        project: ProjectId,
        account: String,
        ids: Vec<RepoId>,
        reply: Result<IssuesReply, GhState>,
        now: Instant,
    ) -> Effects {
        let mut fx = Effects::default();
        let ok = reply.is_ok();
        let stamp = rfc3339((now_ms() / 1000) as i64);
        match reply {
            Ok(reply) => {
                if reply
                    .rate
                    .as_ref()
                    .is_some_and(|r| r.remaining < RATE_FLOOR)
                {
                    self.slow_until.insert(account.clone(), now + poller::SLOW);
                }
                for (id, result) in ids.iter().zip(reply.repos) {
                    if !self.reads_with(*id, &account) {
                        continue;
                    }
                    let read = self.issues.entry(*id).or_default();
                    match result {
                        Ok(list) => {
                            *read = Read {
                                state: GhState::Ok,
                                viewer: Some(reply.viewer.clone()),
                                enabled: list.enabled,
                                issues: list.issues,
                                total: list.total,
                                fetched_at: Some(stamp.clone()),
                                failed_at: None,
                            }
                        }
                        Err(state) => {
                            read.state = state;
                            read.failed_at = Some(stamp.clone());
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
                for id in &ids {
                    if !self.reads_with(*id, &account) {
                        continue;
                    }
                    let read = self.issues.entry(*id).or_default();
                    read.state = state.clone();
                    read.failed_at = Some(stamp.clone());
                }
            }
        }
        let slow = self.slow_until.get(&account).is_some_and(|t| now < *t);
        if let Some(beat) = self.issue_beats.get_mut(&(project, account)) {
            beat.finish(now, ok, if slow { poller::SLOW } else { EVERY });
        }
        self.send_issues(project, To::All, &mut fx);
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::super::query::RepoIssueList;
    use super::super::tests::{World, account, two_projects};
    use super::super::{Done, Effects, Job, To};
    use super::*;
    use std::time::Duration;
    use termist_core::ClientRequest;

    const S: Duration = Duration::from_secs(1);

    fn issue(number: u32, title: &str) -> IssueSummary {
        IssueSummary {
            number,
            title: title.into(),
            url: format!("https://github.com/acme/site/issues/{number}"),
            body: "It breaks.".into(),
            author: "bob".into(),
            labels: vec!["bug".into()],
            assignees: vec![],
            assigned_you: false,
            comments: 0,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-02T10:00:00Z".into(),
        }
    }

    /// Project `i`'s issues looked at by the world's client, or not.
    fn look(w: &mut World, i: usize, issues: bool) -> Effects {
        let project = Some(w.projects[i].id);
        w.request(ClientRequest::SetPrFocus {
            project,
            pr: None,
            diff: false,
            issues,
        })
    }

    /// Issue reads of `fx`: (project index, account, repo names).
    fn reads(w: &World, fx: &Effects) -> Vec<(usize, String, Vec<String>)> {
        fx.jobs
            .iter()
            .filter_map(|j| match j {
                Job::Issues {
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

    /// The issue reads the next tick starts.
    fn ticked(w: &mut World) -> Vec<(usize, String, Vec<String>)> {
        let fx = w.tick();
        reads(w, &fx)
    }

    /// Answers project 0's read by "work": `first` in site, nothing in admin.
    fn answer(w: &mut World, first: Vec<IssueSummary>) -> Effects {
        let ids = vec![w.id("site"), w.id("admin")];
        let project = w.projects[0].id;
        let total = first.len() as u32;
        w.done(Done::Issues {
            project,
            account: "work".into(),
            ids,
            reply: Ok(IssuesReply {
                viewer: "alice".into(),
                rate: None,
                repos: vec![
                    Ok(RepoIssueList {
                        enabled: true,
                        issues: first,
                        total,
                    }),
                    Ok(RepoIssueList {
                        enabled: false,
                        issues: vec![],
                        total: 0,
                    }),
                ],
            }),
        })
    }

    /// The issues of the last `Issues` event to `to`: (folder, enabled, numbers).
    fn sent(fx: &Effects, to: To) -> Option<Vec<(String, bool, Vec<u32>)>> {
        fx.events.iter().rev().find_map(|(t, e)| match e {
            ServerEvent::Issues { repos, .. } if *t == to => Some(
                repos
                    .iter()
                    .map(|r| {
                        (
                            r.name.clone(),
                            r.enabled,
                            r.issues.iter().map(|i| i.number).collect(),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
    }

    #[test]
    fn issues_are_read_only_while_someone_looks_at_them() {
        let mut w = two_projects();
        assert!(ticked(&mut w).is_empty(), "nobody looks");
        look(&mut w, 0, true);
        let fx = w.tick();
        assert_eq!(
            reads(&w, &fx),
            [(
                0,
                "work".to_string(),
                vec!["admin".to_string(), "site".to_string()]
            )]
        );
        assert!(ticked(&mut w).is_empty(), "not twice");
        answer(&mut w, vec![issue(123, "Login")]);
        w.now += 59 * S;
        assert!(ticked(&mut w).is_empty());
        w.now += S;
        assert_eq!(ticked(&mut w).len(), 1, "every minute");
        answer(&mut w, vec![]);
        look(&mut w, 0, false);
        w.now += 600 * S;
        assert!(ticked(&mut w).is_empty(), "not after the look");
    }

    #[test]
    fn what_was_read_goes_to_those_who_look() {
        let mut w = two_projects();
        let other = ClientId(2);
        w.join(other);
        look(&mut w, 0, true);
        w.tick();
        let fx = answer(&mut w, vec![issue(123, "Login"), issue(7, "Docs")]);
        assert_eq!(
            sent(&fx, To::One(w.client)),
            Some(vec![
                ("admin".to_string(), false, vec![]),
                ("site".to_string(), true, vec![123, 7]),
            ])
        );
        assert_eq!(sent(&fx, To::One(other)), None, "it does not look");
        // A second look gets what was read at once.
        let fx = w.gh.request(
            other,
            ClientRequest::SetPrFocus {
                project: Some(w.projects[0].id),
                pr: None,
                diff: false,
                issues: true,
            },
            &w.store,
            &w.projects,
            w.now,
        );
        assert_eq!(
            sent(&fx, To::One(other)).map(|r| r[1].2.clone()),
            Some(vec![123, 7])
        );
    }

    #[test]
    fn refresh_reads_the_issues_again_now() {
        let mut w = two_projects();
        look(&mut w, 0, true);
        w.tick();
        answer(&mut w, vec![]);
        w.now += 5 * S;
        w.request(ClientRequest::RefreshPrs {
            project: w.projects[0].id,
        });
        assert_eq!(ticked(&mut w).len(), 1);
    }

    #[test]
    fn a_failed_read_keeps_the_last_issues_and_says_when() {
        let mut w = two_projects();
        look(&mut w, 0, true);
        w.tick();
        answer(&mut w, vec![issue(123, "Login")]);
        w.now += 60 * S;
        w.tick();
        let ids = vec![w.id("site"), w.id("admin")];
        let project = w.projects[0].id;
        let fx = w.done(Done::Issues {
            project,
            account: "work".into(),
            ids,
            reply: Err(GhState::Failed("timeout".into())),
        });
        let repos = fx
            .events
            .iter()
            .find_map(|(_, e)| match e {
                ServerEvent::Issues { repos, .. } => Some(repos.clone()),
                _ => None,
            })
            .unwrap();
        let site = repos.iter().find(|r| r.name == "site").unwrap();
        assert_eq!(site.issues.len(), 1, "the last list stays");
        assert_eq!(site.state, GhState::Failed("timeout".into()));
        assert!(site.failed_at.is_some() && site.fetched_at.is_some());
        // Failed: read again later than a minute.
        w.now += 60 * S;
        assert!(ticked(&mut w).is_empty());
    }

    #[test]
    fn a_repo_no_account_can_read_says_so_in_the_issues_too() {
        let mut w = super::super::tests::world(&["work"]);
        w.ready(vec![account("work", true)]);
        w.found(0, &[("site", "acme", "site")]);
        w.permit(&[("site", &[("work", None, true)])]);
        let fx = look(&mut w, 0, true);
        let state = fx.events.iter().find_map(|(_, e)| match e {
            ServerEvent::Issues { repos, .. } => Some(repos[0].state.clone()),
            _ => None,
        });
        assert_eq!(state, Some(GhState::NoAccess));
        assert!(ticked(&mut w).is_empty(), "nothing to read it with");
    }
}
