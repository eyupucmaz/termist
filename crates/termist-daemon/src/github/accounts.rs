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

/// The github.com accounts `gh auth status --json hosts` lists as working.
pub fn parse_status(json: &str) -> Vec<(String, bool)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return vec![];
    };
    v["hosts"]["github.com"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["state"].as_str() == Some("success"))
        .filter_map(|a| {
            Some((
                a["login"].as_str()?.to_string(),
                a["active"].as_bool().unwrap_or(false),
            ))
        })
        .collect()
}

/// Every working account with its token.
pub fn load(gh: &dyn Gh) -> Result<Vec<Account>, GhState> {
    let status = gh.run(&["auth", "status", "--json", "hosts"], None, None)?;
    let listed = parse_status(&status.stdout);
    if listed.is_empty() {
        if status.stderr.contains("unknown flag") {
            return Err(GhState::Failed(
                "this gh is too old for termist (no `auth status --json`); upgrade gh".into(),
            ));
        }
        return Err(GhState::LoggedOut);
    }
    let mut accounts = Vec::new();
    for (login, active) in listed {
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
            {"login":"stale","active":false,"state":"error","host":"github.com"}],
        "ghe.acme.com":[{"login":"corp","active":true,"state":"success"}]}}"#;

    #[test]
    fn status_lists_the_working_github_accounts() {
        assert_eq!(
            parse_status(STATUS),
            [
                ("alice".to_string(), true),
                ("alice-work".to_string(), false)
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
