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
    /// The GitHub CLI; default `gh` from PATH (or the login shell).
    pub gh_bin: Option<String>,
}

impl DaemonConfig {
    pub fn from_env() -> DaemonConfig {
        DaemonConfig {
            shell: std::env::var("TERMIST_SHELL").ok(),
            claude_bin: std::env::var("TERMIST_CLAUDE_BIN").ok(),
            codex_bin: std::env::var("TERMIST_CODEX_BIN").ok(),
            opencode_bin: std::env::var("TERMIST_OPENCODE_BIN").ok(),
            gh_bin: std::env::var("TERMIST_GH_BIN").ok(),
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

    pub fn set(&mut self, harness: Harness, program: String) {
        match harness {
            Harness::Claude => self.claude = program,
            Harness::Codex => self.codex = program,
            Harness::OpenCode => self.opencode = program,
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
    /// Passed as the CLI's model flag; `None` passes no flag.
    pub model: Option<&'a str>,
    /// Passed as the CLI's effort flag (Claude, Codex); `None` passes no flag.
    pub effort: Option<&'a str>,
    pub cwd: &'a Path,
    pub cols: u16,
    pub rows: u16,
    pub resume: Option<&'a str>,
    /// Told to an agent: termist's commands (`[agents] teach`); `None` tells nothing.
    pub teach: Option<&'a str>,
    /// A folder an agent may work in besides `cwd`: its repo's worktrees.
    pub also: Option<&'a Path>,
}

pub struct Launch {
    pub spec: SpawnSpec,
    /// Known up front for Claude (`--session-id`); resume passes it back.
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
            SessionKind::Tool { program, args } => (program.clone(), args.clone(), None),
            SessionKind::Agent {
                harness: Harness::Claude,
            } => {
                let mut args = vec![
                    "--settings".to_string(),
                    self.claude_settings.display().to_string(),
                ];
                if let Some(dir) = req.also {
                    args.extend(["--add-dir".to_string(), dir.display().to_string()]);
                }
                if let Some(words) = req.teach {
                    args.extend(["--append-system-prompt".to_string(), words.to_string()]);
                }
                if let Some(m) = req.model {
                    args.extend(["--model".to_string(), m.to_string()]);
                }
                if let Some(e) = req.effort {
                    args.extend(["--effort".to_string(), e.to_string()]);
                }
                let sid = match req.resume {
                    Some(id) => {
                        args.extend(["--resume".to_string(), id.to_string()]);
                        id.to_string()
                    }
                    None => {
                        let sid = uuid::Uuid::new_v4().to_string();
                        args.extend(["--session-id".to_string(), sid.clone()]);
                        if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                            args.extend(["--".to_string(), claude_prompt(p)]);
                        }
                        sid
                    }
                };
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
                crate::codex::args(
                    &self.exe,
                    crate::codex::Launched {
                        resume: req.resume,
                        model: req.model,
                        effort: req.effort,
                        prompt: req.prompt,
                        also: req.also,
                        teach: req.teach,
                    },
                ),
                req.resume.map(str::to_string),
            ),
            SessionKind::Agent {
                harness: Harness::OpenCode,
            } => {
                env.extend(crate::opencode::config_env(
                    &self.opencode_config_dir,
                    std::env::var_os("OPENCODE_CONFIG_DIR").as_deref(),
                    std::env::var_os("OPENCODE_CONFIG_CONTENT").as_deref(),
                    crate::opencode::extra(
                        &self.opencode_config_dir,
                        req.teach.is_some(),
                        req.also,
                    ),
                ));
                (
                    self.programs.get(Harness::OpenCode).to_string(),
                    crate::opencode::args(req.resume, req.model, req.prompt),
                    req.resume.map(str::to_string),
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

/// The prompt as Claude's positional argument, after `--` so a leading `-` is not read
/// as a flag. Claude still runs a command whose name is the first operand, even after
/// `--`, so a one-word prompt (`update`, `doctor`) gets a trailing space that keeps it
/// from matching one.
fn claude_prompt(prompt: &str) -> String {
    if prompt.contains(char::is_whitespace) {
        prompt.to_string()
    } else {
        format!("{prompt} ")
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
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
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
            "hooks must reach the daemon that spawned them"
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
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
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
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
        });
        assert_eq!(l.spec.program, "/fake/claude");
        let a = &l.spec.args;
        assert_eq!(&a[..2], ["--settings", "/data/claude-hooks.json"]);
        assert_eq!(a[2], "--session-id");
        let sid = l.agent_session_id.clone().unwrap();
        assert_eq!(a[3], sid);
        assert!(uuid::Uuid::parse_str(&sid).is_ok());
        assert_eq!(&a[a.len() - 2..], ["--", "fix the login redirect"]);
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
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
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
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
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
    fn opencode_runs_with_our_config_dir_and_the_prompt_as_a_flag() {
        let kind = SessionKind::Agent {
            harness: Harness::OpenCode,
        };
        let l = launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: Some("fix it"),
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: None,
            teach: None,
            also: None,
        });
        assert_eq!(l.spec.program, "/fake/opencode");
        assert_eq!(l.spec.args, ["--prompt=fix it"]);
        let dir = env(&l, "OPENCODE_CONFIG_DIR").or_else(|| env(&l, "OPENCODE_CONFIG_CONTENT"));
        assert!(dir.is_some_and(|d| d.contains("/data/opencode")));
    }

    fn told(harness: Harness, resume: Option<&str>) -> Launch {
        launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &SessionKind::Agent { harness },
            prompt: Some("fix it"),
            model: None,
            effort: None,
            cwd: Path::new("/w/site"),
            cols: 80,
            rows: 24,
            resume,
            teach: Some(termist_core::agents::TEACH),
            also: Some(Path::new("/w/site-worktrees")),
        })
    }

    #[test]
    fn an_agent_is_told_of_termist_and_may_work_in_its_repo_s_worktrees() {
        let c = told(Harness::Claude, None);
        let at = |a: &str| c.spec.args.iter().position(|x| x == a).unwrap();
        assert_eq!(c.spec.args[at("--add-dir") + 1], "/w/site-worktrees");
        assert_eq!(
            c.spec.args[at("--append-system-prompt") + 1],
            termist_core::agents::TEACH
        );
        assert!(at("--append-system-prompt") < at("--"), "before the prompt");
        let resumed = told(Harness::Claude, Some("c-1"));
        assert!(
            resumed
                .spec
                .args
                .contains(&"--append-system-prompt".to_string())
        );
        let x = told(Harness::Codex, None);
        assert!(x.spec.args.contains(&"--add-dir".to_string()));
        assert!(
            x.spec
                .args
                .iter()
                .any(|a| a.starts_with("developer_instructions="))
        );
        let o = told(Harness::OpenCode, None);
        let content: serde_json::Value =
            serde_json::from_str(&env(&o, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(
            content["instructions"],
            serde_json::json!([crate::opencode::teach_file(Path::new("/data/opencode"))
                .display()
                .to_string()])
        );
        assert_eq!(
            content["permission"]["external_directory"]["/w/site-worktrees/**"],
            "allow"
        );
        assert_eq!(
            env(&o, "OPENCODE_CONFIG_DIR").as_deref(),
            Some("/data/opencode")
        );
    }

    fn resumed(kind: SessionKind, id: &str) -> Launch {
        launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &kind,
            prompt: Some("ignored"),
            model: None,
            effort: None,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume: Some(id),
            teach: None,
            also: None,
        })
    }

    #[test]
    fn each_harness_resumes_with_its_own_flag() {
        let c = resumed(
            SessionKind::Agent {
                harness: Harness::Claude,
            },
            "c-1",
        );
        assert_eq!(
            c.spec.args,
            ["--settings", "/data/claude-hooks.json", "--resume", "c-1"]
        );
        assert_eq!(c.agent_session_id.as_deref(), Some("c-1"));

        let x = resumed(
            SessionKind::Agent {
                harness: Harness::Codex,
            },
            "x-1",
        );
        assert_eq!(&x.spec.args[..2], ["resume", "x-1"]);
        assert!(!x.spec.args.contains(&"ignored".to_string()));
        assert_eq!(x.agent_session_id.as_deref(), Some("x-1"));

        let o = resumed(
            SessionKind::Agent {
                harness: Harness::OpenCode,
            },
            "ses_1",
        );
        assert_eq!(o.spec.args, ["--session", "ses_1"]);
        assert_eq!(o.agent_session_id.as_deref(), Some("ses_1"));

        let s = resumed(SessionKind::Shell, "whatever");
        assert!(s.spec.args.is_empty());
        assert_eq!(s.agent_session_id, None);
    }

    fn with_model(
        harness: Harness,
        model: &str,
        effort: Option<&str>,
        resume: Option<&str>,
    ) -> Launch {
        launcher().launch(LaunchRequest {
            id: SessionId::new(),
            kind: &SessionKind::Agent { harness },
            prompt: Some("fix it"),
            model: Some(model),
            effort,
            cwd: Path::new("/p"),
            cols: 80,
            rows: 24,
            resume,
            teach: None,
            also: None,
        })
    }

    #[test]
    fn claude_gets_model_and_effort_before_its_session_flags() {
        let l = with_model(Harness::Claude, "opus", Some("xhigh"), None);
        assert_eq!(
            &l.spec.args[..6],
            [
                "--settings",
                "/data/claude-hooks.json",
                "--model",
                "opus",
                "--effort",
                "xhigh"
            ]
        );
        assert_eq!(l.spec.args[6], "--session-id");
        assert_eq!(l.spec.args.last().unwrap(), "fix it");
    }

    #[test]
    fn resume_passes_the_model_and_effort_again() {
        let c = with_model(Harness::Claude, "opus", Some("max"), Some("c-1"));
        assert_eq!(
            c.spec.args,
            [
                "--settings",
                "/data/claude-hooks.json",
                "--model",
                "opus",
                "--effort",
                "max",
                "--resume",
                "c-1"
            ]
        );
        let x = with_model(Harness::Codex, "gpt-5", Some("low"), Some("x-1"));
        assert_eq!(
            &x.spec.args[..6],
            [
                "resume",
                "x-1",
                "-m",
                "gpt-5",
                "-c",
                "model_reasoning_effort=\"low\""
            ]
        );
        let o = with_model(Harness::OpenCode, "openai/gpt-5", None, Some("ses_1"));
        assert_eq!(o.spec.args, ["--session", "ses_1", "-m", "openai/gpt-5"]);
    }

    fn prompted(harness: Harness, prompt: &str) -> Vec<String> {
        launcher()
            .launch(LaunchRequest {
                id: SessionId::new(),
                kind: &SessionKind::Agent { harness },
                prompt: Some(prompt),
                model: None,
                effort: None,
                cwd: Path::new("/p"),
                cols: 80,
                rows: 24,
                resume: None,
                teach: None,
                also: None,
            })
            .spec
            .args
    }

    // A pasted bullet list or a prompt about a flag is still the prompt, and a one-word
    // prompt that names one of the CLI's commands does not run that command.
    #[test]
    fn a_prompt_that_looks_like_a_flag_or_a_command_stays_the_prompt() {
        let tail = |args: &[String]| args[args.len() - 2..].to_vec();
        for harness in [Harness::Claude, Harness::Codex] {
            assert_eq!(
                tail(&prompted(harness, "-v flag is broken")),
                ["--", "-v flag is broken"],
                "{harness:?}"
            );
            assert_eq!(
                tail(&prompted(harness, "- fix the login\n- add a test")),
                ["--", "- fix the login\n- add a test"],
                "{harness:?}"
            );
        }
        assert_eq!(
            tail(&prompted(Harness::Claude, "update")),
            ["--", "update "],
            "Claude runs a command named by its first word even after --"
        );
        assert_eq!(tail(&prompted(Harness::Codex, "update")), ["--", "update"]);
        assert_eq!(
            prompted(Harness::OpenCode, "-v flag is broken"),
            ["--prompt=-v flag is broken"]
        );
        assert_eq!(prompted(Harness::OpenCode, "update"), ["--prompt=update"]);
    }

    // A model name typed by the user goes to the CLI as one argument, exactly as
    // typed: there is no shell in between to split or unquote it.
    #[test]
    fn a_model_name_with_spaces_and_quotes_is_one_argument() {
        let odd = r#"my "odd" model's name"#;
        for (harness, flag) in [
            (Harness::Claude, "--model"),
            (Harness::Codex, "-m"),
            (Harness::OpenCode, "-m"),
        ] {
            let args = with_model(harness, odd, None, None).spec.args;
            let at = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{harness:?}: no {flag} in {args:?}"));
            assert_eq!(args[at + 1], odd, "{harness:?}");
        }
    }
}
