//! `termist update`: installs the newest release over this one, through the release's
//! own installer (the one `install.sh` runs), into the directory it was installed in.
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const REPO: &str = "eyupucmaz/termist";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A version as tags and Cargo write it: `0.1.0`, `0.1.0-alpha.2`, with or without `v`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches('v');
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, pre.split('.').map(str::to_string).collect()),
            None => (text, vec![]),
        };
        // Only what SemVer allows: the tag also goes into the installer's URL.
        let identifier =
            |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !pre.iter().all(|p| identifier(p)) {
            return None;
        }
        let mut parts = core.split('.').map(|p| {
            Some(p)
                .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|p| p.parse::<u64>().ok())
        });
        let core = [parts.next()??, parts.next()??, parts.next()??];
        parts.next().is_none().then_some(Version { core, pre })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c] = self.core;
        write!(f, "{a}.{b}.{c}")?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    /// SemVer order: a release is newer than its pre-releases; pre-release parts
    /// compare as numbers when both are numbers, else as text, numbers first.
    fn cmp(&self, other: &Version) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The newest release's tag, pre-releases included (as `install.sh` finds it).
fn latest_tag() -> Result<String, String> {
    let mut curl = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    curl.args(["--proto", "=https", "--tlsv1.2", "-fsSL"])
        .args(["-H", "Accept: application/vnd.github+json"]);
    // With a token (as in CI) the API allows far more than 60 lookups an hour.
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.is_empty()
    {
        curl.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    let out = curl
        .arg(format!(
            "https://api.github.com/repos/{REPO}/releases?per_page=1"
        ))
        .output()
        .map_err(|e| format!("could not run curl to ask GitHub for releases ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "GitHub did not answer: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let releases: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("GitHub's answer is not JSON ({e})"))?;
    releases
        .get(0)
        .and_then(|r| r.get("tag_name"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{REPO} has no release yet"))
}

/// Where the installer put termist, from the receipt it leaves: the binary's path.
fn installed_by_installer() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("XDG_CONFIG_HOME")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
    }?;
    let receipt =
        std::fs::read_to_string(home.join("termist").join("termist-receipt.json")).ok()?;
    receipt_binary(&receipt)
}

/// The binary a cargo-dist install receipt describes.
fn receipt_binary(receipt: &str) -> Option<PathBuf> {
    let receipt: serde_json::Value = serde_json::from_str(receipt).ok()?;
    let prefix = PathBuf::from(receipt.get("install_prefix")?.as_str()?);
    let exe = if cfg!(windows) {
        "termist.exe"
    } else {
        "termist"
    };
    // "cargo-home" (the default, ~/.cargo) and "hierarchical" keep binaries in bin/.
    let flat = receipt.get("install_layout").and_then(|l| l.as_str()) == Some("flat");
    Some(if flat {
        prefix.join(exe)
    } else {
        prefix.join("bin").join(exe)
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

const INSTALL_HINT: &str = if cfg!(windows) {
    "powershell -ExecutionPolicy Bypass -c \"irm https://eyupucmaz.github.io/termist/install.ps1 | iex\""
} else {
    "curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh"
};

pub fn run(check_only: bool) -> ExitCode {
    let current = Version::parse(CURRENT).expect("the package version");
    let tag = match latest_tag() {
        Ok(tag) => tag,
        Err(e) => {
            eprintln!("termist: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(latest) = Version::parse(&tag) else {
        eprintln!("termist: the newest release has a tag termist cannot read: {tag:?}");
        return ExitCode::FAILURE;
    };
    // The tag names the download, so it must be exactly the version it reads as.
    if tag.strip_prefix('v').unwrap_or(&tag) != latest.to_string() {
        eprintln!("termist: the newest release has a tag termist cannot read: {tag:?}");
        return ExitCode::FAILURE;
    }
    if latest <= current {
        println!("termist {current} is up to date (the newest release is {latest})");
        return ExitCode::SUCCESS;
    }
    if check_only {
        println!("termist {latest} is out (this is {current}); `termist update` installs it");
        return ExitCode::SUCCESS;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let prefix = match installed_by_installer() {
        Some(installed) if same_file(&installed, &exe) => installed
            .parent()
            .map(|bin| {
                if bin.file_name().is_some_and(|n| n == "bin") {
                    bin.parent().unwrap_or(bin).to_path_buf()
                } else {
                    bin.to_path_buf()
                }
            })
            .unwrap_or_default(),
        _ => {
            eprintln!(
                "termist: this termist ({}) was not put there by termist's installer, so \
                 `termist update` leaves it alone. Update it the way you installed it, or \
                 install the newest release with:\n  {INSTALL_HINT}",
                exe.display()
            );
            return ExitCode::FAILURE;
        }
    };
    println!("termist: updating {current} to {latest}");
    if let Err(e) = install(&tag, &prefix, &exe) {
        eprintln!("termist: the update failed: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "termist: updated to {latest}. The daemon still runs {current} and keeps your \
         sessions: when they can stop, run `termist kill`, then `termist` starts the new one."
    );
    ExitCode::SUCCESS
}

/// Runs the release's installer for `tag`, into `prefix` (its PATH set-up untouched).
/// The URL reaches the shell through the environment, never inside the command line.
fn install(tag: &str, prefix: &Path, exe: &Path) -> Result<(), String> {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    #[cfg(unix)]
    let status = {
        let _ = exe;
        Command::new("sh")
            .arg("-c")
            .arg("curl --proto '=https' --tlsv1.2 -LsSf \"$TERMIST_INSTALLER_URL\" | sh")
            .env(
                "TERMIST_INSTALLER_URL",
                format!("{base}/termist-installer.sh"),
            )
            .env("TERMIST_INSTALL_DIR", prefix)
            .env("TERMIST_NO_MODIFY_PATH", "1")
            .status()
    };
    #[cfg(windows)]
    let status = {
        // A running .exe cannot be overwritten, but it can be renamed out of the way.
        let old = exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)
            .map_err(|e| format!("could not move {} aside ({e})", exe.display()))?;
        let status = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg("irm $env:TERMIST_INSTALLER_URL | iex")
            .env(
                "TERMIST_INSTALLER_URL",
                format!("{base}/termist-installer.ps1"),
            )
            .env("TERMIST_INSTALL_DIR", prefix)
            .env("TERMIST_NO_MODIFY_PATH", "1")
            .status();
        if !status.as_ref().is_ok_and(|s| s.success()) {
            let _ = std::fs::rename(&old, exe);
        }
        status
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("the installer exited with {s}")),
        Err(e) => Err(format!("could not run the installer ({e})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_are_read_with_or_without_v() {
        assert_eq!(v("v0.1.0-alpha.1"), v("0.1.0-alpha.1"));
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("0.1.0-alpha.1").to_string(), "0.1.0-alpha.1");
        for bad in [
            "",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "v1.2.x",
            "+1.2.3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-a'b",
            "1.2.3-a/../b",
            "1.2.3-a b",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn versions_are_in_semver_order() {
        let ordered = [
            "0.1.0-alpha.1",
            "0.1.0-alpha.2",
            "0.1.0-alpha.10",
            "0.1.0-alpha.beta",
            "0.1.0-beta",
            "0.1.0-beta.2",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(
            v("0.1.0-alpha.1").cmp(&v("v0.1.0-alpha.1")),
            Ordering::Equal
        );
    }

    #[test]
    fn the_receipt_names_the_binary() {
        let exe = if cfg!(windows) {
            "termist.exe"
        } else {
            "termist"
        };
        let receipt = r#"{"binaries":["termist"],"install_layout":"cargo-home","install_prefix":"/home/e/.cargo","version":"0.1.0-alpha.1"}"#;
        assert_eq!(
            receipt_binary(receipt),
            Some(PathBuf::from("/home/e/.cargo").join("bin").join(exe))
        );
        let flat = r#"{"install_layout":"flat","install_prefix":"/opt/termist"}"#;
        assert_eq!(
            receipt_binary(flat),
            Some(PathBuf::from("/opt/termist").join(exe))
        );
        assert_eq!(receipt_binary("{}"), None);
        assert_eq!(receipt_binary("not json"), None);
    }
}
