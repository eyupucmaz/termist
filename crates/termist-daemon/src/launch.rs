use crate::session::SpawnSpec;
use std::path::{Path, PathBuf};
use termist_core::{Harness, SessionId, SessionKind};

#[derive(Clone, Debug, Default)]
pub struct DaemonConfig {
    /// Program for shell sessions; default `$SHELL` (unix) or `powershell.exe` (Windows).
    pub shell: Option<String>,
    /// Program for Claude sessions; default `claude` from PATH.
    pub claude_bin: Option<String>,
}

impl DaemonConfig {
    pub fn from_env() -> DaemonConfig {
        DaemonConfig {
            shell: std::env::var("TERMIST_SHELL").ok(),
            claude_bin: std::env::var("TERMIST_CLAUDE_BIN").ok(),
        }
    }
}

pub struct LaunchRequest<'a> {
    pub id: SessionId,
    pub kind: &'a SessionKind,
    pub prompt: Option<&'a str>,
    pub cwd: &'a Path,
    pub cols: u16,
    pub rows: u16,
}

pub struct Launch {
    pub spec: SpawnSpec,
    /// Known up front for Claude (`--session-id`); used for resume in Plan 2.
    pub agent_session_id: Option<String>,
}

pub struct Launcher {
    pub config: DaemonConfig,
    pub exe: PathBuf,
    pub claude_settings: PathBuf,
    /// The daemon's runtime dir, exported as `TERMIST_RUNTIME_DIR` so `termist hook`
    /// reaches this daemon's socket or pipe whatever the agent's environment says.
    pub runtime_dir: PathBuf,
    /// The daemon's own `TERMIST_HOME`, when it had one; passed on to sessions.
    pub termist_home: Option<PathBuf>,
}

impl Launcher {
    pub fn launch(&self, req: LaunchRequest<'_>) -> Launch {
        let mut env = vec![
            ("TERMIST_SESSION_ID".to_string(), req.id.to_string()),
            ("TERMIST_BIN".to_string(), self.exe.display().to_string()),
            (
                "TERMIST_RUNTIME_DIR".to_string(),
                self.runtime_dir.display().to_string(),
            ),
        ];
        if let Some(home) = &self.termist_home {
            env.push(("TERMIST_HOME".to_string(), home.display().to_string()));
        }
        let (program, args, agent_session_id) = match req.kind {
            SessionKind::Shell => (
                self.config.shell.clone().unwrap_or_else(default_shell),
                vec![],
                None,
            ),
            SessionKind::Agent {
                harness: Harness::Claude,
            } => {
                let sid = uuid::Uuid::new_v4().to_string();
                let mut args = vec![
                    "--settings".to_string(),
                    self.claude_settings.display().to_string(),
                    "--session-id".to_string(),
                    sid.clone(),
                ];
                if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                    args.push(p.to_string());
                }
                (
                    self.config
                        .claude_bin
                        .clone()
                        .unwrap_or_else(|| "claude".into()),
                    args,
                    Some(sid),
                )
            }
        };
        Launch {
            spec: SpawnSpec {
                id: req.id,
                program,
                args,
                cwd: req.cwd.to_path_buf(),
                env,
                cols: req.cols,
                rows: req.rows,
            },
            agent_session_id,
        }
    }
}

fn default_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
    #[cfg(windows)]
    {
        "powershell.exe".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher() -> Launcher {
        Launcher {
            config: DaemonConfig {
                shell: Some("/bin/zsh".into()),
                claude_bin: Some("/fake/claude".into()),
            },
            exe: PathBuf::from("/usr/local/bin/termist"),
            claude_settings: PathBuf::from("/data/claude-hooks.json"),
            runtime_dir: PathBuf::from("/run/termist"),
            termist_home: None,
        }
    }

    fn env(l: &Launch, key: &str) -> Option<String> {
        l.spec
            .env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn a_shell_runs_the_configured_shell_in_the_project() {
        let id = SessionId::new();
        let l = launcher().launch(LaunchRequest {
            id,
            kind: &SessionKind::Shell,
            prompt: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(l.spec.program, "/bin/zsh");
        assert!(l.spec.args.is_empty());
        assert_eq!(l.spec.cwd, PathBuf::from("/p"));
        assert_eq!((l.spec.cols, l.spec.rows), (80, 24));
        assert_eq!(l.agent_session_id, None);
        assert_eq!(env(&l, "TERMIST_SESSION_ID"), Some(id.to_string()));
        assert_eq!(
            env(&l, "TERMIST_BIN").as_deref(),
            Some("/usr/local/bin/termist")
        );
        assert_eq!(
            env(&l, "TERMIST_RUNTIME_DIR").as_deref(),
            Some("/run/termist"),
            "hooks must reach the daemon that spawned them (PRD §11.4)"
        );
        assert_eq!(env(&l, "TERMIST_HOME"), None);
    }

    #[test]
    fn termist_home_is_passed_on_when_the_daemon_has_one() {
        let mut l = launcher();
        l.termist_home = Some(PathBuf::from("/tmp/th"));
        let launch = l.launch(LaunchRequest {
            id: SessionId::new(),
            kind: &SessionKind::Shell,
            prompt: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(env(&launch, "TERMIST_HOME").as_deref(), Some("/tmp/th"));
    }

    #[test]
    fn claude_gets_our_settings_a_preassigned_session_id_and_the_prompt_last() {
        let kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        let l = launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: Some("fix the login redirect"),
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(l.spec.program, "/fake/claude");
        let a = &l.spec.args;
        assert_eq!(&a[..2], ["--settings", "/data/claude-hooks.json"]);
        assert_eq!(a[2], "--session-id");
        let sid = l.agent_session_id.clone().unwrap();
        assert_eq!(a[3], sid);
        assert!(uuid::Uuid::parse_str(&sid).is_ok());
        assert_eq!(a.last().unwrap(), "fix the login redirect");
    }

    #[test]
    fn claude_defaults_to_the_binary_on_path() {
        let mut l = launcher();
        l.config.claude_bin = None;
        let kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        let launch = l.launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(launch.spec.program, "claude");
        assert_eq!(
            launch.spec.args.len(),
            4,
            "no prompt means no positional argument"
        );
    }
}
