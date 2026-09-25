/// Environment variables removed before spawning an agent: they make agent CLIs think
/// they run inside tmux, zellij, another Claude Code, or the host terminal (spike 2026-09-25).
pub fn should_scrub(name: &str) -> bool {
    matches!(
        name,
        "TMUX" | "TMUX_PANE" | "STY" | "CLAUDECODE" | "WT_SESSION"
    ) || name.starts_with("ZELLIJ")
        || name.starts_with("CLAUDE_CODE_")
        || name.starts_with("TERM_PROGRAM")
}

#[cfg(test)]
mod tests {
    use super::should_scrub;

    #[test]
    fn multiplexer_and_nested_agent_variables_are_scrubbed() {
        for v in [
            "TMUX",
            "TMUX_PANE",
            "STY",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "WT_SESSION",
        ] {
            assert!(should_scrub(v), "{v}");
        }
        for v in [
            "PATH",
            "HOME",
            "TERM",
            "ANTHROPIC_API_KEY",
            "TERMIST_SESSION_ID",
        ] {
            assert!(!should_scrub(v), "{v}");
        }
    }
}
