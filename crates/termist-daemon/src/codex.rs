//! Codex hooks, injected with `-c` flags only: nothing is written to ~/.codex or the
//! repo (spike 2026-09-25 §3). Our hooks are trusted through a `hooks.state` entry
//! whose hash Codex recomputes; if the format ever drifts, Codex shows its own review
//! screen instead, and termist never passes `--dangerously-bypass-hook-trust`.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use termist_core::{Harness, Signal};

pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

pub fn snake_case(event: &str) -> String {
    let mut out = String::with_capacity(event.len() + 4);
    for (i, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn default_timeout(snake_event: &str) -> u64 {
    if matches!(snake_event, "session_end" | "interrupt") {
        1
    } else {
        600
    }
}

/// Codex's trusted-hash of one command hook. Keys are written in sorted order so the
/// canonical form holds even if serde_json's `preserve_order` feature is enabled.
pub fn trust_hash(event: &str, command: &str) -> String {
    let snake = snake_case(event);
    let identity = json!({
        "event_name": snake,
        "hooks": [{
            "async": false,
            "command": command,
            "timeout": default_timeout(&snake),
            "type": "command",
        }],
    });
    let canonical = serde_json::to_string(&identity).expect("json value serializes");
    let digest = Sha256::digest(canonical.as_bytes());
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// A TOML basic string.
pub fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `codex [resume <id>] -c hooks.… -c hooks.state.… [prompt]`.
pub fn args(exe: &Path, resume: Option<&str>, prompt: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(id) = resume {
        args.extend(["resume".to_string(), id.to_string()]);
    }
    for ev in EVENTS {
        let cmd = crate::hookcmd::hook_command(exe, Harness::Codex, ev);
        args.push("-c".into());
        args.push(format!(
            "hooks.{ev}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
            toml_string(&cmd)
        ));
        args.push("-c".into());
        args.push(format!(
            "hooks.state.\"/<session-flags>/config.toml:{}:0:0\".trusted_hash=\"{}\"",
            snake_case(ev),
            trust_hash(ev, &cmd)
        ));
    }
    if resume.is_none()
        && let Some(p) = prompt.filter(|p| !p.trim().is_empty())
    {
        args.push(p.to_string());
    }
    args
}

pub fn signal_for(event: &str, _payload: &Value) -> Option<Signal> {
    match event {
        "UserPromptSubmit" => Some(Signal::PromptSubmitted),
        "PermissionRequest" => Some(Signal::NeedsFeedback),
        "PostToolUse" => Some(Signal::ToolDone),
        "Stop" => Some(Signal::TurnStopped),
        "Interrupt" => Some(Signal::Cancelled),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPIKE_BIN: &str = "/private/tmp/claude-501/-Users-eyup-Code-termist/b9b40857-4886-479a-9330-5db012e9ab1c/scratchpad/bin/hooklog";

    #[test]
    fn event_names_become_snake_case() {
        assert_eq!(snake_case("UserPromptSubmit"), "user_prompt_submit");
        assert_eq!(snake_case("Stop"), "stop");
        assert_eq!(snake_case("SessionEnd"), "session_end");
    }

    /// Hashes Codex 0.156.1 itself wrote into config.toml for these exact commands.
    #[test]
    fn trust_hashes_match_the_ones_codex_computed() {
        assert_eq!(
            trust_hash("Stop", &format!("{SPIKE_BIN} CX:Stop")),
            "sha256:70ff015721d34c53546596e5d77d7c42975f83eb3f237f65b3be4b6326f987b8"
        );
        assert_eq!(
            trust_hash("Interrupt", &format!("{SPIKE_BIN} CX:Interrupt")),
            "sha256:10b00d56b15d8f826906c83db8146c8a792bebb9a223100b31a11f34ad39fe7a",
            "interrupt uses the 1 s timeout"
        );
    }

    /// Review Focus 5: every hook carries its own trust entry, computed from the very
    /// same command string, and the bypass flag never appears.
    #[test]
    fn every_hook_is_paired_with_its_trust_entry() {
        let exe = Path::new("/usr/local/bin/termist");
        let args = args(exe, None, Some("fix it"));
        assert!(!args.iter().any(|a| a.contains("dangerously")));
        assert_eq!(args.last().map(String::as_str), Some("fix it"));
        for ev in EVENTS {
            let cmd = crate::hookcmd::hook_command(exe, Harness::Codex, ev);
            let hook = format!(
                "hooks.{ev}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
                toml_string(&cmd)
            );
            let state = format!(
                "hooks.state.\"/<session-flags>/config.toml:{}:0:0\".trusted_hash=\"{}\"",
                snake_case(ev),
                trust_hash(ev, &cmd)
            );
            let hi = args
                .iter()
                .position(|a| *a == hook)
                .unwrap_or_else(|| panic!("missing {hook}"));
            let si = args
                .iter()
                .position(|a| *a == state)
                .unwrap_or_else(|| panic!("missing {state}"));
            assert_eq!(args[hi - 1], "-c");
            assert_eq!(args[si - 1], "-c");
        }
    }

    #[test]
    fn resume_comes_first_and_drops_the_prompt() {
        let args = args(Path::new("/t"), Some("019a-uuid"), Some("ignored"));
        assert_eq!(&args[..2], ["resume", "019a-uuid"]);
        assert!(!args.contains(&"ignored".to_string()));
    }

    #[test]
    fn toml_strings_escape_quotes_and_backslashes() {
        assert_eq!(toml_string(r#"C:\a "b""#), r#""C:\\a \"b\"""#);
    }

    #[test]
    fn codex_events_map_to_signals() {
        let p = json!({});
        assert_eq!(
            signal_for("UserPromptSubmit", &p),
            Some(Signal::PromptSubmitted)
        );
        assert_eq!(
            signal_for("PermissionRequest", &p),
            Some(Signal::NeedsFeedback)
        );
        assert_eq!(signal_for("PostToolUse", &p), Some(Signal::ToolDone));
        assert_eq!(signal_for("Stop", &p), Some(Signal::TurnStopped));
        assert_eq!(signal_for("Interrupt", &p), Some(Signal::Cancelled));
        assert_eq!(signal_for("SessionStart", &p), None);
        assert_eq!(signal_for("SessionEnd", &p), None);
    }
}
