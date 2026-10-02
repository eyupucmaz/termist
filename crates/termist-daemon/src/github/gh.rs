//! Asking GitHub through the `gh` CLI. Every call carries one account's token as
//! GH_TOKEN, so the user never switches accounts; tokens stay in this process.
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use termist_core::github::GhState;
use termist_platform::process::{self, RunError};

/// How long one call may take.
pub const LIMIT: Duration = Duration::from_secs(20);

/// Variables that would point gh at another host than github.com.
const OTHER_HOST: &[&str] = &["GH_HOST", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait Gh: Send + Sync {
    /// `gh args…` with `token` as GH_TOKEN and `stdin` on its input. `Err` when gh
    /// could not be run at all (missing, hung).
    fn run(
        &self,
        args: &[&str],
        token: Option<&str>,
        stdin: Option<&str>,
    ) -> Result<GhOutput, GhState>;
}

/// A `gh` to call, handed to the jobs.
#[derive(Clone)]
pub struct GhHandle(pub Arc<dyn Gh>);

impl std::fmt::Debug for GhHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GhHandle")
    }
}

/// The real CLI.
pub struct CliGh {
    pub program: PathBuf,
}

impl Gh for CliGh {
    fn run(
        &self,
        args: &[&str],
        token: Option<&str>,
        stdin: Option<&str>,
    ) -> Result<GhOutput, GhState> {
        // No prompts, colours or update notices in what we parse.
        let mut env = vec![
            ("GH_PROMPT_DISABLED", "1"),
            ("NO_COLOR", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
        ];
        if let Some(token) = token {
            env.push(("GH_TOKEN", token));
        }
        match process::run_without(&self.program, args, &env, OTHER_HOST, stdin, LIMIT) {
            Ok(out) => Ok(GhOutput {
                success: out.success,
                stdout: out.stdout,
                stderr: out.stderr,
            }),
            Err(RunError::NotFound) => Err(GhState::NoGh),
            Err(RunError::TimedOut) => Err(GhState::Failed("gh did not answer in 20 s".into())),
            Err(RunError::Io(e)) => Err(GhState::Failed(format!("could not run gh: {e}"))),
        }
    }
}

/// Why a call failed, from what gh printed.
pub fn classify(out: &GhOutput) -> GhState {
    let said = format!("{}\n{}", out.stderr, out.stdout).to_lowercase();
    if said.contains("rate limit") {
        return GhState::RateLimited {
            reset_at: String::new(),
        };
    }
    if [
        "bad credentials",
        "http 401",
        "gh auth login",
        "not logged in",
    ]
    .iter()
    .any(|s| said.contains(s))
    {
        return GhState::LoggedOut;
    }
    let line = out
        .stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh failed");
    GhState::Failed(line.trim_start_matches("gh: ").to_string())
}

/// Runs a GraphQL query as `token`, the query on gh's input. GitHub answers some
/// errors next to the data (a repo that is not there): the data counts and the caller
/// reads the gaps.
pub fn graphql(gh: &dyn Gh, token: &str, query: &str) -> Result<Value, GhState> {
    let body = serde_json::json!({ "query": query }).to_string();
    let out = gh.run(
        &["api", "graphql", "--hostname", "github.com", "--input", "-"],
        Some(token),
        Some(&body),
    )?;
    match serde_json::from_str::<Value>(&out.stdout) {
        Ok(v) if v.get("data").is_some_and(|d| !d.is_null()) => Ok(v),
        _ => Err(classify(&out)),
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Call {
        pub args: Vec<String>,
        pub token: Option<String>,
        pub stdin: Option<String>,
    }

    impl Call {
        /// The GraphQL query this call sent, if it sent one.
        pub fn query(&self) -> String {
            self.stdin
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| v["query"].as_str().map(str::to_string))
                .unwrap_or_default()
        }
    }

    type Answer = dyn Fn(&Call) -> Result<GhOutput, GhState> + Send + Sync;

    /// A `gh` that answers from a function and remembers every call.
    pub struct FakeGh {
        answer: Box<Answer>,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeGh {
        pub fn new(
            answer: impl Fn(&Call) -> Result<GhOutput, GhState> + Send + Sync + 'static,
        ) -> Arc<FakeGh> {
            Arc::new(FakeGh {
                answer: Box::new(answer),
                calls: Mutex::new(vec![]),
            })
        }

        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Gh for FakeGh {
        fn run(
            &self,
            args: &[&str],
            token: Option<&str>,
            stdin: Option<&str>,
        ) -> Result<GhOutput, GhState> {
            let call = Call {
                args: args.iter().map(|a| a.to_string()).collect(),
                token: token.map(str::to_string),
                stdin: stdin.map(str::to_string),
            };
            self.calls.lock().unwrap().push(call.clone());
            (self.answer)(&call)
        }
    }

    pub fn ok(stdout: &str) -> Result<GhOutput, GhState> {
        Ok(GhOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    pub fn fails(stdout: &str, stderr: &str) -> Result<GhOutput, GhState> {
        Ok(GhOutput {
            success: false,
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeGh, fails, ok};
    use super::*;

    fn out(stdout: &str, stderr: &str) -> GhOutput {
        GhOutput {
            success: false,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn failures_are_told_apart() {
        let bad = out(
            r#"{"message":"Bad credentials","status":"401"}"#,
            "gh: Bad credentials (HTTP 401)",
        );
        assert_eq!(classify(&bad), GhState::LoggedOut);
        let login = out(
            "",
            "To get started with GitHub CLI, please run:  gh auth login",
        );
        assert_eq!(classify(&login), GhState::LoggedOut);
        let rate = out("", "gh: API rate limit exceeded for user ID 1. (HTTP 403)");
        assert_eq!(
            classify(&rate),
            GhState::RateLimited {
                reset_at: String::new()
            }
        );
        let other = out(
            "",
            "gh: Could not resolve to a Repository with the name 'a/b'.\n",
        );
        assert_eq!(
            classify(&other),
            GhState::Failed("Could not resolve to a Repository with the name 'a/b'.".into())
        );
        assert_eq!(classify(&out("", "")), GhState::Failed("gh failed".into()));
    }

    #[test]
    fn graphql_sends_the_query_on_stdin_with_the_token() {
        let gh = FakeGh::new(|_| ok(r#"{"data":{"viewer":{"login":"alice"}}}"#));
        let v = graphql(&*gh, "tok-alice", "query { viewer { login } }").unwrap();
        assert_eq!(v["data"]["viewer"]["login"], "alice");
        let calls = gh.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].args,
            ["api", "graphql", "--hostname", "github.com", "--input", "-"]
        );
        assert_eq!(calls[0].token.as_deref(), Some("tok-alice"));
        assert_eq!(calls[0].query(), "query { viewer { login } }");
    }

    #[test]
    fn partial_errors_keep_the_data() {
        let gh = FakeGh::new(|_| {
            fails(
                r#"{"data":{"r0":null},"errors":[{"type":"NOT_FOUND","path":["r0"]}]}"#,
                "gh: Could not resolve to a Repository",
            )
        });
        let v = graphql(&*gh, "t", "q").unwrap();
        assert!(v["data"]["r0"].is_null());
    }

    #[test]
    fn an_answer_that_is_not_json_is_classified() {
        let gh = FakeGh::new(|_| fails("<html>502 Bad Gateway</html>", "gh: HTTP 502\n"));
        assert_eq!(
            graphql(&*gh, "t", "q"),
            Err(GhState::Failed("HTTP 502".into()))
        );
        let gh = FakeGh::new(|_| {
            ok(
                r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
            )
        });
        assert!(matches!(
            graphql(&*gh, "t", "q"),
            Err(GhState::RateLimited { .. })
        ));
    }

    #[test]
    fn a_missing_gh_stays_missing() {
        let gh = FakeGh::new(|_| Err(GhState::NoGh));
        assert_eq!(graphql(&*gh, "t", "q"), Err(GhState::NoGh));
    }

    #[cfg(unix)]
    #[test]
    fn the_cli_passes_the_token_and_reports_a_missing_program() {
        let missing = CliGh {
            program: "/nonexistent/gh".into(),
        };
        assert_eq!(missing.run(&["--version"], None, None), Err(GhState::NoGh));
        let echo = CliGh {
            program: "/bin/sh".into(),
        };
        let out = echo
            .run(&["-c", "printf %s \"$GH_TOKEN\""], Some("tok"), None)
            .unwrap();
        assert_eq!(out.stdout, "tok");
        // Never another host's, whatever the daemon inherited.
        let script = OTHER_HOST
            .iter()
            .map(|v| format!("${{{v}-unset}}"))
            .collect::<Vec<_>>()
            .join(" ");
        let out = echo
            .run(&["-c", &format!("printf %s \"{script}\"")], None, None)
            .unwrap();
        assert_eq!(out.stdout, "unset unset unset");
    }
}
