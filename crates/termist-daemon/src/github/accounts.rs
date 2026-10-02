//! The GitHub accounts gh is logged in to, their tokens, and which one reads a repo.
use super::gh::Gh;
use serde_json::Value;
use termist_core::github::GhState;

/// What an account may do in a repo, least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

impl Permission {
    /// GitHub's `viewerPermission` value.
    pub fn parse(s: &str) -> Option<Permission> {
        Some(match s {
            "READ" => Permission::Read,
            "TRIAGE" => Permission::Triage,
            "WRITE" => Permission::Write,
            "MAINTAIN" => Permission::Maintain,
            "ADMIN" => Permission::Admin,
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    pub login: String,
    /// gh's active account: it wins a tie.
    pub active: bool,
    pub token: String,
}

/// Never prints the token.
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("login", &self.login)
            .field("active", &self.active)
            .field("token", &"…")
            .finish()
    }
}

/// What `gh auth status` found out about an account's token. gh checks each token
/// with GitHub, so offline every account comes back failed, not only a bad one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    Works,
    /// GitHub turned the token down: logged out.
    Refused,
    /// GitHub could not be asked (offline, timed out): the token may well work.
    Unreachable,
}

/// The github.com accounts `gh auth status --json hosts` lists: login, active, and
/// how its token fared.
pub fn parse_status(json: &str) -> Vec<(String, bool, Checked)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return vec![];
    };
    v["hosts"]["github.com"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let login = a["login"].as_str().filter(|l| !l.is_empty())?;
            let error = a["error"].as_str().unwrap_or("").to_lowercase();
            let checked = match a["state"].as_str() {
                Some("success") => Checked::Works,
                Some("timeout") => Checked::Unreachable,
                _ if ["401", "bad credentials", "unauthorized", "token"]
                    .iter()
                    .any(|s| error.contains(s)) =>
                {
                    Checked::Refused
                }
                Some("error") => Checked::Unreachable,
                _ => Checked::Refused,
            };
            Some((
                login.to_string(),
                a["active"].as_bool().unwrap_or(false),
                checked,
            ))
        })
        .collect()
}

/// Every account whose token was not turned down, with its token.
pub fn load(gh: &dyn Gh) -> Result<Vec<Account>, GhState> {
    let status = gh.run(&["auth", "status", "--json", "hosts"], None, None)?;
    let listed = parse_status(&status.stdout);
    if listed.is_empty() && status.stderr.contains("unknown flag") {
        return Err(GhState::Failed(
            "this gh is too old for termist (no `auth status --json`); upgrade gh".into(),
        ));
    }
    if !listed.iter().any(|(_, _, c)| *c == Checked::Works) {
        return Err(
            if listed.iter().any(|(_, _, c)| *c == Checked::Unreachable) {
                GhState::Failed("could not reach GitHub".into())
            } else {
                GhState::LoggedOut
            },
        );
    }
    let mut accounts = Vec::new();
    for (login, active, _) in listed
        .into_iter()
        .filter(|(_, _, c)| *c != Checked::Refused)
    {
        let out = gh.run(
            &[
                "auth",
                "token",
                "--hostname",
                "github.com",
                "--user",
                &login,
            ],
            None,
            None,
        )?;
        let token = out.stdout.trim();
        if out.success && !token.is_empty() {
            accounts.push(Account {
                login,
                active,
                token: token.to_string(),
            });
        }
    }
    if accounts.is_empty() {
        Err(GhState::LoggedOut)
    } else {
        Ok(accounts)
    }
}

/// The account to read a repo with: the most access wins, gh's active account breaks
/// a tie. `None` when no account sees the repo.
pub fn pick(seen: &[(String, Option<Permission>, bool)]) -> Option<String> {
    seen.iter()
        .filter_map(|(login, perm, active)| Some(((*perm)?, *active, login)))
        .max_by_key(|(perm, active, _)| (*perm, *active))
        .map(|(_, _, login)| login.clone())
}

#[cfg(test)]
mod tests {
    use super::super::gh::fake::{FakeGh, ok};
    use super::*;

    const STATUS: &str = r#"{"hosts":{
        "github.com":[
            {"login":"alice","active":true,"state":"success","host":"github.com"},
            {"login":"alice-work","active":false,"state":"success","host":"github.com"},
            {"login":"stale","active":false,"state":"error","host":"github.com",
             "error":"non-200 OK status code: 401 Unauthorized body: \"Bad credentials\""},
            {"login":"","active":false,"state":"error","host":"github.com","tokenSource":"GH_TOKEN"}],
        "ghe.acme.com":[{"login":"corp","active":true,"state":"success"}]}}"#;

    #[test]
    fn status_lists_the_github_accounts_and_how_their_tokens_fared() {
        assert_eq!(
            parse_status(STATUS),
            [
                ("alice".to_string(), true, Checked::Works),
                ("alice-work".to_string(), false, Checked::Works),
                ("stale".to_string(), false, Checked::Refused),
            ]
        );
        assert!(parse_status(r#"{"hosts":{}}"#).is_empty());
        assert!(parse_status("not json").is_empty());
    }

    #[test]
    fn load_takes_each_accounts_token_and_never_prints_it() {
        let gh = FakeGh::new(|call| {
            if call.args[1] == "status" {
                ok(STATUS)
            } else {
                ok(&format!("tok-{}\n", call.args.last().unwrap()))
            }
        });
        let accounts = load(&*gh).unwrap();
        assert_eq!(
            accounts,
            [
                Account {
                    login: "alice".into(),
                    active: true,
                    token: "tok-alice".into()
                },
                Account {
                    login: "alice-work".into(),
                    active: false,
                    token: "tok-alice-work".into()
                },
            ]
        );
        assert!(!format!("{accounts:?}").contains("tok-"));
        let token_call = &gh.calls()[1];
        assert_eq!(
            token_call.args,
            [
                "auth",
                "token",
                "--hostname",
                "github.com",
                "--user",
                "alice"
            ]
        );
    }

    #[test]
    fn no_account_is_logged_out() {
        let gh = FakeGh::new(|_| ok(r#"{"hosts":{}}"#));
        assert_eq!(load(&*gh), Err(GhState::LoggedOut));
        let gone = FakeGh::new(|_| Err(GhState::NoGh));
        assert_eq!(load(&*gone), Err(GhState::NoGh));
    }

    /// What gh 2.x prints offline: every token is checked with GitHub and fails.
    const OFFLINE: &str = r#"{"hosts":{"github.com":[
        {"state":"error","error":"Get \"https://api.github.com/\": dial tcp: lookup api.github.com: no such host",
         "active":true,"host":"github.com","login":"alice","tokenSource":"keyring"},
        {"state":"timeout","error":"context deadline exceeded",
         "active":false,"host":"github.com","login":"alice-work","tokenSource":"keyring"}]}}"#;

    #[test]
    fn offline_is_a_failure_to_retry_soon_not_logged_out() {
        let gh = FakeGh::new(|_| ok(OFFLINE));
        assert_eq!(
            load(&*gh),
            Err(GhState::Failed("could not reach GitHub".into()))
        );
        assert_eq!(gh.calls().len(), 1, "no token read");
        let refused = r#"{"hosts":{"github.com":[{"state":"error","active":true,
            "error":"HTTP 401: Bad credentials (https://api.github.com/)","login":"alice"}]}}"#;
        let gh = FakeGh::new(move |_| ok(refused));
        assert_eq!(load(&*gh), Err(GhState::LoggedOut));
    }

    #[test]
    fn an_account_that_timed_out_next_to_a_working_one_still_reads() {
        let mixed = r#"{"hosts":{"github.com":[
            {"state":"success","active":true,"login":"alice"},
            {"state":"timeout","active":false,"login":"alice-work"},
            {"state":"error","active":false,"login":"stale","error":"HTTP 401: Bad credentials"}]}}"#;
        let gh = FakeGh::new(move |call| {
            if call.args[1] == "status" {
                ok(mixed)
            } else {
                ok(&format!("tok-{}\n", call.args.last().unwrap()))
            }
        });
        let logins: Vec<String> = load(&*gh).unwrap().into_iter().map(|a| a.login).collect();
        assert_eq!(logins, ["alice", "alice-work"]);
    }

    #[test]
    fn the_most_access_wins_and_the_active_account_breaks_a_tie() {
        use Permission::*;
        let s = |login: &str, p: Option<Permission>, active: bool| (login.to_string(), p, active);
        // The owner reads their own repo, although the work account is active.
        assert_eq!(
            pick(&[s("work", Some(Read), true), s("me", Some(Admin), false)]),
            Some("me".into())
        );
        // A work repo the personal account cannot see.
        assert_eq!(
            pick(&[s("work", Some(Write), true), s("me", None, false)]),
            Some("work".into())
        );
        assert_eq!(
            pick(&[s("me", Some(Write), false), s("work", Some(Write), true)]),
            Some("work".into())
        );
        assert_eq!(pick(&[s("me", None, true), s("work", None, false)]), None);
        assert_eq!(Permission::parse("MAINTAIN"), Some(Maintain));
        assert_eq!(Permission::parse("owner"), None);
    }
}
