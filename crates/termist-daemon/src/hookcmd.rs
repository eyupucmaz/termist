//! The one command string every harness runs for a hook.
use std::path::Path;
use termist_core::Harness;

pub fn hook_command(exe: &Path, harness: Harness, event: &str) -> String {
    format!(
        "{} hook --harness {} {event}",
        quote(&exe.display().to_string()),
        harness.id()
    )
}

#[cfg(unix)]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn quote(s: &str) -> String {
    format!("\"{s}\"")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn every_harness_uses_the_same_quoted_form() {
        let exe = Path::new("/a b/it's/termist");
        assert_eq!(
            hook_command(exe, Harness::Codex, "Stop"),
            "'/a b/it'\\''s/termist' hook --harness codex Stop"
        );
        assert_eq!(
            hook_command(exe, Harness::OpenCode, "x"),
            "'/a b/it'\\''s/termist' hook --harness opencode x"
        );
    }
}
