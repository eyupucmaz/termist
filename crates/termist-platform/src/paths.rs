use std::io;
use std::path::PathBuf;

/// Unix domain socket paths are limited (104 bytes on macOS, 108 on Linux).
const MAX_SOCKET_PATH: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub runtime_dir: PathBuf,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    /// `TERMIST_HOME` puts everything under one directory (tests, isolated setups);
    /// otherwise XDG-style dirs on every platform (PRD §11.7).
    pub fn from_env() -> anyhow::Result<Paths> {
        if let Some(home) = std::env::var_os("TERMIST_HOME") {
            return Ok(Paths::under(PathBuf::from(home)));
        }
        use etcetera::BaseStrategy;
        let base = etcetera::choose_base_strategy()?;
        Ok(Paths {
            runtime_dir: default_runtime_dir(),
            data_dir: base.data_dir().join("termist"),
            config_dir: base.config_dir().join("termist"),
        })
    }

    pub fn under(root: PathBuf) -> Paths {
        Paths {
            runtime_dir: root.join("run"),
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        }
    }

    pub fn ensure(&self) -> io::Result<()> {
        for dir in [&self.runtime_dir, &self.data_dir, &self.config_dir] {
            std::fs::create_dir_all(dir)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.runtime_dir, std::fs::Permissions::from_mode(0o700))?;
            self.check_socket_path_len()?;
        }
        Ok(())
    }

    pub fn check_socket_path_len(&self) -> io::Result<()> {
        let len = self.socket_path().as_os_str().len();
        if len > MAX_SOCKET_PATH {
            return Err(io::Error::other(format!(
                "socket path {} is too long ({len} bytes); set TERMIST_HOME to a shorter directory",
                self.socket_path().display()
            )));
        }
        Ok(())
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("daemon.sock")
    }

    /// Windows named pipe name: one per runtime dir, so isolated homes never collide.
    pub fn pipe_name(&self) -> String {
        let mut h: u64 = 0xcbf29ce484222325; // FNV-1a
        for b in self.runtime_dir.to_string_lossy().as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("termist-{h:016x}")
    }

    pub fn claude_settings_path(&self) -> PathBuf {
        self.data_dir.join("claude-hooks.json")
    }

    pub fn daemon_log_path(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("termist.db")
    }

    /// termist's own OpenCode config dir (layered on top of the user's config).
    pub fn opencode_config_dir(&self) -> PathBuf {
        self.data_dir.join("opencode")
    }
}

fn default_runtime_dir() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            return PathBuf::from(dir).join("termist");
        }
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("termist-{uid}"))
    }
    #[cfg(windows)]
    {
        std::env::temp_dir().join("termist")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_puts_everything_below_one_root() {
        let p = Paths::under(PathBuf::from("/x"));
        assert_eq!(p.runtime_dir, PathBuf::from("/x/run"));
        assert_eq!(p.data_dir, PathBuf::from("/x/data"));
        assert_eq!(
            p.claude_settings_path(),
            PathBuf::from("/x/data/claude-hooks.json")
        );
    }

    #[test]
    fn pipe_name_is_stable_and_differs_per_root() {
        let a = Paths::under(PathBuf::from("/a"));
        let b = Paths::under(PathBuf::from("/b"));
        assert_eq!(a.pipe_name(), Paths::under(PathBuf::from("/a")).pipe_name());
        assert_ne!(a.pipe_name(), b.pipe_name());
        assert!(a.pipe_name().starts_with("termist-"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_makes_the_runtime_dir_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::under(tmp.path().to_path_buf());
        p.ensure().unwrap();
        let mode = std::fs::metadata(&p.runtime_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(p.data_dir.is_dir() && p.config_dir.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_refuses_a_socket_path_too_long_for_the_os() {
        let p = Paths::under(PathBuf::from(format!("/tmp/{}", "x".repeat(120))));
        let err = p.check_socket_path_len().unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn data_files_live_under_the_data_dir() {
        let p = Paths::under(PathBuf::from("/x"));
        assert_eq!(p.db_path(), PathBuf::from("/x/data/termist.db"));
        assert_eq!(p.opencode_config_dir(), PathBuf::from("/x/data/opencode"));
    }
}
