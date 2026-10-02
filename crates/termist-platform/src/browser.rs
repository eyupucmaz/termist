//! Opening a web page in the user's browser.
use std::process::{Command, Stdio};

/// Opens `url`. Only `https://` addresses are opened: they come from GitHub's answers,
/// and nothing else should reach the system's opener.
pub fn open(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("not a web address: {url}"));
    }
    let mut child = opener(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open the browser: {e}"))?;
    // Reaped on a thread: the opener returns at once, the browser lives on.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn opener(url: &str) -> Command {
    let mut cmd = Command::new("open");
    cmd.arg(url);
    cmd
}

#[cfg(windows)]
fn opener(url: &str) -> Command {
    let mut cmd = Command::new("explorer");
    cmd.arg(url);
    cmd
}

#[cfg(all(unix, not(target_os = "macos")))]
fn opener(url: &str) -> Command {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_opened() {
        for bad in [
            "file:///etc/passwd",
            "http://example.com",
            "javascript:alert(1)",
            "https://example.com/a b",
            "https://example.com/\n",
            "",
        ] {
            assert!(open(bad).is_err(), "{bad:?}");
        }
    }
}
