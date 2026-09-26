use crate::session::SpawnSpec;
use std::path::{Path, PathBuf};
use termist_core::{Harness, HarnessInfo, SessionId, SessionKind};

#[derive(Clone, Debug, Default)]
pub struct DaemonConfig {
    /// Program for shell sessions; default `$SHELL` (unix) or `powershell.exe` (Windows).
    pub shell: Option<String>,
    /// Program for Claude sessions; default `claude` from PATH.
    pub claude_bin: Option<String>,
    /// Program for Codex sessions; default `codex` from PATH.
    pub codex_bin: Option<String>,
    /// Program for OpenCode sessions; default `opencode` from PATH.
    pub opencode_bin: Option<String>,
}

impl DaemonConfig {
    pub fn from_env() -> DaemonConfig {
        DaemonConfig {
            shell: std::env::var("TERMIST_SHELL").ok(),
            claude_bin: std::env::var("TERMIST_CLAUDE_BIN").ok(),
            codex_bin: std::env::var("TERMIST_CODEX_BIN").ok(),
            opencode_bin: std::env::var("TERMIST_OPENCODE_BIN").ok(),
        }
    }
}

/// The program each harness launches: a configured `TERMIST_*_BIN`, else the path the
/// resolver found, else the bare name (spawning it then fails with a clear error).
#[derive(Clone, Debug)]
pub struct HarnessPrograms {
    pub claude: String,
    pub codex: String,
    pub opencode: String,
}

impl HarnessPrograms {
    pub fn get(&self, harness: Harness) -> &str {
        match harness {
            Harness::Claude => &self.claude,
            Harness::Codex => &self.codex,
            Harness::OpenCode => &self.opencode,
        }
    }

    pub fn resolve(config: &DaemonConfig) -> (HarnessPrograms, Vec<HarnessInfo>) {
        Self::resolve_with(config, crate::resolve::find_program)
    }

    /// Looks the three CLIs up in parallel: a missing one can cost a login-shell start.
    pub fn resolve_with(
        config: &DaemonConfig,
        find: impl Fn(&str) -> Option<PathBuf> + Sync,
    ) -> (HarnessPrograms, Vec<HarnessInfo>) {
        let found: Vec<(String, bool)> = std::thread::scope(|scope| {
            let handles: Vec<_> = Harness::ALL
                .iter()
                .map(|&harness| {
                    let find = &find;
                    let configured = configured_bin(config, harness);
                    scope.spawn(move || match configured {
                        Some(bin) => (bin.clone(), true),
                        None => match find(harness.program()) {
                            Some(path) => (path.display().to_string(), true),
                            None => (harness.program().to_string(), false),
                        },
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("resolver thread"))
                .collect()
        });
        let infos = Harness::ALL
            .iter()
            .zip(&found)
            .map(|(&harness, (_, available))| HarnessInfo {
                harness,
                available: *available,
            })
            .collect();
        let [claude, codex, opencode]: [String; 3] = found
            .into_iter()
            .map(|(program, _)| program)
            .collect::<Vec<_>>()
            .try_into()
            .expect("one program per harness");
        (
            HarnessPrograms {
                claude,
                codex,
                opencode,
            },
            infos,
        )
    }
}

fn configured_bin(config: &DaemonConfig, harness: Harness) -> &Option<String> {
    match harness {
        Harness::Claude => &config.claude_bin,
        Harness::Codex => &config.codex_bin,
        Harness::OpenCode => &config.opencode_bin,
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
    pub programs: HarnessPrograms,
    pub exe: PathBuf,
    pub claude_settings: PathBuf,
    pub opencode_config_dir: PathBuf,
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
                    self.programs.get(Harness::Claude).to_string(),
                    args,
                    Some(sid),
                )
            }
            SessionKind::Agent {
                harness: Harness::Codex,
            } => (
                self.programs.get(Harness::Codex).to_string(),
                crate::codex::args(&self.exe, None, req.prompt),
                None,
            ),
            SessionKind::Agent {
                harness: Harness::OpenCode,
            } => {
                env.extend(crate::opencode::config_env(
                    &self.opencode_config_dir,
                    std::env::var_os("OPENCODE_CONFIG_DIR").as_deref(),
                ));
                (
                    self.programs.get(Harness::OpenCode).to_string(),
                    crate::opencode::args(None),
                    None,
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
                ..Default::default()
            },
            programs: HarnessPrograms {
                claude: "/fake/claude".into(),
                codex: "/fake/codex".into(),
                opencode: "/fake/opencode".into(),
            },
            exe: PathBuf::from("/usr/local/bin/termist"),
            claude_settings: PathBuf::from("/data/claude-hooks.json"),
            opencode_config_dir: PathBuf::from("/data/opencode"),
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
    fn claude_without_a_prompt_has_no_positional_argument() {
        let kind = SessionKind::Agent {
            harness: Harness::Claude,
        };
        let launch = launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(
            launch.spec.args.len(),
            4,
            "no prompt means no positional argument"
        );
    }

    #[test]
    fn a_configured_binary_wins_and_missing_clis_are_reported() {
        use termist_core::HarnessInfo;

        let config = DaemonConfig {
            claude_bin: Some("/opt/claude".into()),
            ..Default::default()
        };
        let (programs, infos) = HarnessPrograms::resolve_with(&config, |name| {
            (name == "codex").then(|| PathBuf::from("/usr/local/bin/codex"))
        });
        assert_eq!(programs.get(Harness::Claude), "/opt/claude");
        assert_eq!(programs.get(Harness::Codex), "/usr/local/bin/codex");
        assert_eq!(
            programs.get(Harness::OpenCode),
            "opencode",
            "unresolved CLIs keep their bare name"
        );
        assert_eq!(
            infos,
            vec![
                HarnessInfo {
                    harness: Harness::Claude,
                    available: true
                },
                HarnessInfo {
                    harness: Harness::Codex,
                    available: true
                },
                HarnessInfo {
                    harness: Harness::OpenCode,
                    available: false
                },
            ]
        );
    }

    #[test]
    fn codex_gets_hook_flags_and_no_preassigned_id() {
        let kind = SessionKind::Agent {
            harness: Harness::Codex,
        };
        let l = launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(l.spec.program, "/fake/codex");
        assert_eq!(l.spec.args[0], "-c");
        assert!(l.spec.args.iter().any(|a| a.starts_with("hooks.Stop=")));
        assert_eq!(
            l.agent_session_id, None,
            "codex reports its id in the first SessionStart hook"
        );
    }

    #[test]
    fn opencode_runs_with_our_config_dir() {
        let kind = SessionKind::Agent {
            harness: Harness::OpenCode,
        };
        let l = launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: Some("ignored for now"),
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
        });
        assert_eq!(l.spec.program, "/fake/opencode");
        assert!(l.spec.args.is_empty());
        let dir = env(&l, "OPENCODE_CONFIG_DIR").or_else(|| env(&l, "OPENCODE_CONFIG_CONTENT"));
        assert!(dir.is_some_and(|d| d.contains("/data/opencode")));
    }
}
