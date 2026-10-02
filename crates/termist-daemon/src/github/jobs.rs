//! What each job does: the gh calls, on a blocking thread.
use super::gh::{self, GhHandle};
use super::{Done, Job, Slug, accounts, query, repos};
use std::sync::Arc;
use termist_core::github::GhState;

/// Finds the gh to call: the configured one, else on PATH or through the login shell.
pub type Locate = Arc<dyn Fn() -> Option<GhHandle> + Send + Sync>;

pub fn run(job: Job, locate: &Locate) -> Done {
    match job {
        Job::Accounts => Done::Accounts(match locate() {
            None => Err(GhState::NoGh),
            Some(gh) => accounts::load(&*gh.0).map(|list| (gh, list)),
        }),
        Job::Discover { project, path } => Done::Discovered {
            project,
            repos: repos::discover(&path, &repos::git),
        },
        Job::Permissions {
            gh,
            accounts,
            repos,
        } => {
            let q = query::permissions(&names(&repos));
            let mut seen = vec![Vec::new(); repos.len()];
            let mut failure = None;
            for a in &accounts {
                match gh::graphql(&*gh.0, &a.token, &q) {
                    Ok(v) => {
                        for (i, p) in query::parse_permissions(&v, repos.len())
                            .into_iter()
                            .enumerate()
                        {
                            seen[i].push((a.login.clone(), p, a.active));
                        }
                    }
                    Err(e) => failure = Some(e),
                }
            }
            Done::Permissions(match failure {
                Some(e) if seen.iter().all(Vec::is_empty) => Err(e),
                _ => Ok(repos.into_iter().map(|(id, ..)| id).zip(seen).collect()),
            })
        }
        Job::Inbox {
            gh,
            project,
            account,
            repos,
        } => {
            let reply = gh::graphql(&*gh.0, &account.token, &query::inbox(&names(&repos)))
                .map(|v| query::parse_inbox(&v, repos.len()));
            Done::Inbox {
                project,
                account: account.login,
                ids: repos.into_iter().map(|(id, ..)| id).collect(),
                reply,
            }
        }
        Job::Counts {
            gh,
            project,
            batches,
        } => {
            let mut counts = Vec::new();
            for (account, repos) in batches {
                let found = gh::graphql(&*gh.0, &account.token, &query::counts(&names(&repos)))
                    .map(|v| query::parse_counts(&v, repos.len()))
                    .unwrap_or_else(|_| vec![None; repos.len()]);
                counts.extend(repos.into_iter().map(|(id, ..)| id).zip(found));
            }
            Done::Counts { project, counts }
        }
        Job::Detail {
            gh,
            pr,
            account,
            owner,
            name,
        } => Done::Detail {
            pr,
            reply: gh::graphql(
                &*gh.0,
                &account.token,
                &query::detail(&owner, &name, pr.number),
            )
            .and_then(|v| query::parse_detail(&v)),
        },
    }
}

fn names(repos: &[Slug]) -> Vec<(String, String)> {
    repos
        .iter()
        .map(|(_, owner, name)| (owner.clone(), name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::accounts::{Account, Permission};
    use super::super::gh::fake::{FakeGh, fails, ok};
    use super::super::query::tests::{DETAIL, INBOX};
    use super::*;
    use termist_core::ProjectId;
    use termist_core::github::{PrRef, RepoId};

    fn account(login: &str, active: bool) -> Account {
        Account {
            login: login.into(),
            active,
            token: format!("tok-{login}"),
        }
    }

    fn slugs(names: &[&str]) -> Vec<Slug> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (RepoId(i as i64 + 1), "acme".to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn accounts_without_gh_are_no_gh() {
        let locate: Locate = Arc::new(|| None);
        assert!(matches!(
            run(Job::Accounts, &locate),
            Done::Accounts(Err(GhState::NoGh))
        ));
    }

    #[test]
    fn an_inbox_job_reads_its_repos_with_its_accounts_token() {
        let fake = FakeGh::new(|_| fails(INBOX, "gh: Could not resolve to a Repository"));
        let locate: Locate = Arc::new(|| None);
        let project = ProjectId::new();
        let done = run(
            Job::Inbox {
                gh: GhHandle(fake.clone()),
                project,
                account: account("alice", true),
                repos: slugs(&["site", "gone"]),
            },
            &locate,
        );
        let Done::Inbox {
            account,
            ids,
            reply,
            ..
        } = done
        else {
            panic!()
        };
        assert_eq!(account, "alice");
        assert_eq!(ids, [RepoId(1), RepoId(2)]);
        let reply = reply.unwrap();
        assert_eq!(reply.repos[0].as_ref().unwrap().0.len(), 2);
        assert_eq!(reply.repos[1], Err(GhState::NoAccess));
        let call = &fake.calls()[0];
        assert_eq!(call.token.as_deref(), Some("tok-alice"));
        assert!(
            call.query()
                .contains(r#"r1: repository(owner: "acme", name: "gone")"#)
        );
    }

    #[test]
    fn permissions_ask_every_account() {
        let fake = FakeGh::new(|call| match call.token.as_deref() {
            Some("tok-work") => ok(r#"{"data":{"r0":{"viewerPermission":"WRITE"}}}"#),
            _ => fails(
                r#"{"data":{"r0":null},"errors":[{"type":"NOT_FOUND"}]}"#,
                "gh: Could not resolve",
            ),
        });
        let done = run(
            Job::Permissions {
                gh: GhHandle(fake),
                accounts: vec![account("work", true), account("me", false)],
                repos: slugs(&["site"]),
            },
            &(Arc::new(|| None) as Locate),
        );
        let Done::Permissions(Ok(results)) = done else {
            panic!()
        };
        assert_eq!(
            results,
            [(
                RepoId(1),
                vec![
                    ("work".to_string(), Some(Permission::Write), true),
                    ("me".to_string(), None, false)
                ]
            )]
        );
    }

    #[test]
    fn permissions_that_all_fail_are_a_failure() {
        let fake = FakeGh::new(|_| fails("", "gh: HTTP 502"));
        let done = run(
            Job::Permissions {
                gh: GhHandle(fake),
                accounts: vec![account("work", true)],
                repos: slugs(&["site"]),
            },
            &(Arc::new(|| None) as Locate),
        );
        assert!(matches!(done, Done::Permissions(Err(GhState::Failed(_)))));
    }

    #[test]
    fn a_detail_job_reads_one_pull_request() {
        let fake = FakeGh::new(|_| ok(DETAIL));
        let pr = PrRef {
            repo: RepoId(1),
            number: 212,
        };
        let done = run(
            Job::Detail {
                gh: GhHandle(fake.clone()),
                pr,
                account: account("alice", true),
                owner: "acme".into(),
                name: "site".into(),
            },
            &(Arc::new(|| None) as Locate),
        );
        let Done::Detail { reply: Ok(d), .. } = done else {
            panic!()
        };
        assert_eq!(d.threads.len(), 2);
        assert!(fake.calls()[0].query().contains("pullRequest(number: 212)"));
    }
}
