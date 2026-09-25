/// Environment variables removed before spawning an agent: they make agent CLIs think
/// they run inside tmux, zellij, the host terminal, or a nested Claude Code session
/// (spike 2026-09-25). Only the nested-session markers of Claude Code are removed
/// (`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SSE_PORT`); every other
/// `CLAUDE_CODE_*` variable is user configuration (Bedrock, Vertex, OAuth token, …)
/// and is passed through (controller ruling R13).
pub fn should_scrub(name: &str) -> bool {
    matches!(
        name,
        "TMUX"
            | "TMUX_PANE"
            | "STY"
            | "CLAUDECODE"
            | "CLAUDE_CODE_ENTRYPOINT"
            | "CLAUDE_CODE_SSE_PORT"
            | "WT_SESSION"
    ) || name.starts_with("ZELLIJ")
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
            "CLAUDE_CODE_SSE_PORT",
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

    #[test]
    fn claude_code_user_configuration_is_kept() {
        for v in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(!should_scrub(v), "{v} is user configuration");
        }
    }
}
