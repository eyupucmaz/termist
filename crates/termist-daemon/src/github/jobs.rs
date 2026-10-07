//! What each job does: the gh calls, on a blocking thread.
use super::gh::{self, GhHandle};
use super::{Done, Job, Slug, accounts, files, query, repos, worktree, write};
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
            let ask = || -> Result<Vec<_>, GhState> {
                let mut seen = vec![Vec::new(); repos.len()];
                // One account that could not answer would read as "no access" and pick
                // another reader: the whole question is asked again instead.
                for a in &accounts {
                    let v = gh::graphql(&*gh.0, &a.token, &q)?;
                    for (i, p) in query::parse_permissions(&v, repos.len())
                        .into_iter()
                        .enumerate()
                    {
                        seen[i].push((a.login.clone(), p, a.active));
                    }
                }
                Ok(seen)
            };
            Done::Permissions(
                ask().map(|seen| repos.iter().map(|(id, ..)| *id).zip(seen).collect()),
            )
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
        Job::Diff {
            gh,
            pr,
            account,
            want,
        } => Done::Diff {
            pr,
            reply: files::fetch(&*gh.0, &account.token, &want),
            head_oid: want.head_oid,
        },
        Job::Worktree {
            gh,
            pr,
            account,
            repo,
            head,
        } => Done::Worktree {
            pr,
            reply: worktree::open(&repo, &head, &worktree::git, &*gh.0, &account.token),
        },
        Job::Write {
            gh,
            pr,
            account,
            at,
            write: w,
            client,
            ticket,
        } => Done::Written {
            pr,
            client,
            ticket,
            what: write::what(&w),
            reply: write::send(&*gh.0, &account.token, &w, &at),
        },
        Job::MarkViewed {
            gh,
            pr,
            account,
            id,
            path,
            viewed,
            client,
        } => Done::Marked {
            pr,
            reply: files::mark(&*gh.0, &account.token, &id, &path, viewed),
            path,
            viewed,
            client,
        },
    }
}

/// What `job` reports when it could not finish (it panicked): a failure of its kind,
/// so its beat is not left in flight.
pub fn failed(job: &Job) -> Done {
    let why = || GhState::Failed("internal error".into());
    match job {
        Job::Accounts => Done::Accounts(Err(why())),
        Job::Discover { project, .. } => Done::Undiscovered { project: *project },
        Job::Permissions { .. } => Done::Permissions(Err(why())),
        Job::Inbox {
            project,
            account,
            repos,
            ..
        } => Done::Inbox {
            project: *project,
            account: account.login.clone(),
            ids: repos.iter().map(|(id, ..)| *id).collect(),
            reply: Err(why()),
        },
        Job::Counts {
            project, batches, ..
        } => Done::Counts {
            project: *project,
            counts: batches
                .iter()
                .flat_map(|(_, repos)| repos.iter().map(|(id, ..)| (*id, None)))
                .collect(),
        },
        Job::Detail { pr, .. } => Done::Detail {
            pr: *pr,
            reply: Err(why()),
        },
        Job::Diff { pr, want, .. } => Done::Diff {
            pr: *pr,
            head_oid: want.head_oid.clone(),
            reply: Err(why()),
        },
        Job::Worktree { pr, .. } => Done::Worktree {
            pr: *pr,
            reply: Err("internal error".into()),
        },
        Job::Write {
            pr,
            write: w,
            client,
            ticket,
            ..
        } => Done::Written {
            pr: *pr,
            client: *client,
            ticket: *ticket,
            what: write::what(w),
            reply: Err(why()),
        },
        Job::MarkViewed {
            pr,
            path,
            viewed,
            client,
            ..
        } => Done::Marked {
            pr: *pr,
            path: path.clone(),
            viewed: *viewed,
            client: *client,
            reply: Err(why()),
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
    fn one_account_failing_is_a_failure() {
        let fake = FakeGh::new(|call| match call.token.as_deref() {
            Some("tok-work") => ok(r#"{"data":{"r0":{"viewerPermission":"WRITE"}}}"#),
            _ => fails("", "gh: HTTP 502"),
        });
        let done = run(
            Job::Permissions {
                gh: GhHandle(fake),
                accounts: vec![account("work", true), account("me", false)],
                repos: slugs(&["site"]),
            },
            &(Arc::new(|| None) as Locate),
        );
        assert!(
            matches!(done, Done::Permissions(Err(GhState::Failed(_)))),
            "{done:?}"
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
    fn a_job_that_could_not_finish_fails_as_its_kind() {
        let gh = || GhHandle(FakeGh::new(|_| ok("")));
        let project = ProjectId::new();
        let internal = GhState::Failed("internal error".into());
        let pr = PrRef {
            repo: RepoId(1),
            number: 212,
        };
        assert!(matches!(failed(&Job::Accounts), Done::Accounts(Err(ref e)) if *e == internal));
        assert!(matches!(
            failed(&Job::Discover {
                project,
                path: "/code".into()
            }),
            Done::Undiscovered { project: p } if p == project
        ));
        assert!(matches!(
            failed(&Job::Permissions {
                gh: gh(),
                accounts: vec![account("work", true)],
                repos: slugs(&["site"]),
            }),
            Done::Permissions(Err(_))
        ));
        let Done::Inbox {
            project: p,
            account: login,
            ids,
            reply,
        } = failed(&Job::Inbox {
            gh: gh(),
            project,
            account: account("work", true),
            repos: slugs(&["site", "admin"]),
        })
        else {
            panic!()
        };
        assert_eq!(
            (p, login.as_str(), ids),
            (project, "work", vec![RepoId(1), RepoId(2)])
        );
        assert_eq!(reply.unwrap_err(), internal);
        let Done::Counts { counts, .. } = failed(&Job::Counts {
            gh: gh(),
            project,
            batches: vec![(account("work", true), slugs(&["site", "admin"]))],
        }) else {
            panic!()
        };
        assert_eq!(counts, [(RepoId(1), None), (RepoId(2), None)]);
        assert!(matches!(
            failed(&Job::Detail {
                gh: gh(),
                pr,
                account: account("work", true),
                owner: "acme".into(),
                name: "site".into(),
            }),
            Done::Detail { pr: p, reply: Err(_) } if p == pr
        ));
    }

    fn want() -> files::Want {
        files::Want {
            owner: "acme".into(),
            name: "site".into(),
            number: 212,
            url: "https://github.com/acme/site/pull/212".into(),
            changed: 3,
            head_oid: "h1".into(),
        }
    }

    #[test]
    fn a_diff_job_reads_files_and_viewed_states_as_its_account() {
        use super::super::files::tests::{PAGE, VIEWED};
        let fake = FakeGh::new(|c| {
            if c.args.contains(&"graphql".to_string()) {
                ok(VIEWED)
            } else {
                ok(PAGE)
            }
        });
        let pr = PrRef {
            repo: RepoId(1),
            number: 212,
        };
        let done = run(
            Job::Diff {
                gh: GhHandle(fake.clone()),
                pr,
                account: account("alice", true),
                want: want(),
            },
            &(Arc::new(|| None) as Locate),
        );
        let Done::Diff {
            pr: p,
            head_oid,
            reply: Ok(d),
        } = done
        else {
            panic!()
        };
        assert_eq!((p, head_oid.as_str(), d.files.len()), (pr, "h1", 3));
        assert!(
            fake.calls()
                .iter()
                .all(|c| c.token.as_deref() == Some("tok-alice"))
        );
    }

    #[test]
    fn a_mark_job_answers_the_client_that_asked() {
        let fake =
            FakeGh::new(|_| ok(r#"{"data":{"markFileAsViewed":{"clientMutationId":null}}}"#));
        let pr = PrRef {
            repo: RepoId(1),
            number: 212,
        };
        let client = crate::session::ClientId(4);
        let done = run(
            Job::MarkViewed {
                gh: GhHandle(fake.clone()),
                pr,
                account: account("alice", true),
                id: "PR_1".into(),
                path: "src/a.rs".into(),
                viewed: true,
                client,
            },
            &(Arc::new(|| None) as Locate),
        );
        assert!(matches!(
            done,
            Done::Marked { client: c, viewed: true, reply: Ok(()), ref path, .. }
                if c == client && path == "src/a.rs"
        ));
        assert!(fake.calls()[0].query().contains("markFileAsViewed"));
        let Done::Marked { reply, .. } = failed(&Job::MarkViewed {
            gh: GhHandle(fake),
            pr,
            account: account("alice", true),
            id: "PR_1".into(),
            path: "src/a.rs".into(),
            viewed: true,
            client,
        }) else {
            panic!()
        };
        assert!(reply.is_err());
        assert!(matches!(
            failed(&Job::Diff {
                gh: GhHandle(FakeGh::new(|_| ok(""))),
                pr,
                account: account("alice", true),
                want: want(),
            }),
            Done::Diff { reply: Err(_), ref head_oid, .. } if head_oid == "h1"
        ));
    }

    #[test]
    fn a_write_job_writes_as_its_account_and_says_what_it_was() {
        let fake = FakeGh::new(|_| ok(r#"{"data":{"addComment":{"clientMutationId":null}}}"#));
        let pr = PrRef {
            repo: RepoId(1),
            number: 212,
        };
        let client = crate::session::ClientId(3);
        let job = |gh: GhHandle| Job::Write {
            gh,
            pr,
            account: account("alice", true),
            at: write::Spot {
                id: "PR_1".into(),
                owner: "acme".into(),
                name: "site".into(),
                number: 212,
            },
            write: termist_core::github::PrWrite::Comment { body: "hi".into() },
            client,
            ticket: 5,
        };
        let done = run(job(GhHandle(fake.clone())), &(Arc::new(|| None) as Locate));
        assert!(matches!(
            done,
            Done::Written { ticket: 5, client: c, what: "post your comment", reply: Ok(()), .. } if c == client
        ));
        assert_eq!(fake.calls()[0].token.as_deref(), Some("tok-alice"));
        assert!(matches!(
            failed(&job(GhHandle(fake))),
            Done::Written {
                ticket: 5,
                reply: Err(_),
                ..
            }
        ));
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
