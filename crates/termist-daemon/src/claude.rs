use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use termist_core::Signal;
use termist_platform::Paths;

/// Claude Code hook events termist listens to.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

const ATTENTION_NOTIFICATIONS: &[&str] = &[
    "permission_prompt",
    "elicitation_dialog",
    "agent_needs_input",
];

pub fn hook_command(exe: &Path, event: &str) -> String {
    crate::hookcmd::hook_command(exe, termist_core::Harness::Claude, event)
}

/// The file passed with `claude --settings`. Claude merges these hooks with the
/// user's own; nothing is written into the user's repository.
pub fn settings_json(exe: &Path) -> Value {
    let mut hooks = Map::new();
    for ev in EVENTS {
        hooks.insert(
            ev.to_string(),
            json!([{ "hooks": [{ "type": "command", "command": hook_command(exe, ev), "timeout": 5 }] }]),
        );
    }
    json!({ "hooks": hooks })
}

pub fn write_settings(paths: &Paths, exe: &Path) -> anyhow::Result<PathBuf> {
    let path = paths.claude_settings_path();
    std::fs::write(&path, serde_json::to_vec_pretty(&settings_json(exe))?)?;
    Ok(path)
}

pub fn signal_for(event: &str, payload: &Value) -> Option<Signal> {
    match event {
        "UserPromptSubmit" => Some(Signal::PromptSubmitted),
        "PermissionRequest" => Some(Signal::NeedsFeedback),
        "Notification" => {
            let kind = payload
                .get("notification_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ATTENTION_NOTIFICATIONS
                .contains(&kind)
                .then_some(Signal::NeedsFeedback)
        }
        "PostToolUse" => Some(Signal::ToolDone),
        "Stop" | "StopFailure" => Some(Signal::TurnStopped),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_event_gets_one_termist_hook_command() {
        let exe = Path::new("/opt/My Tools/termist");
        let v = settings_json(exe);
        for ev in EVENTS {
            let cmd = v["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap();
            assert!(
                cmd.ends_with(&format!("hook --harness claude {ev}")),
                "{cmd}"
            );
            assert_eq!(v["hooks"][ev][0]["hooks"][0]["type"], "command");
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_paths_with_spaces_and_quotes_survive_the_shell() {
        assert_eq!(
            hook_command(Path::new("/a b/it's/termist"), "Stop"),
            "'/a b/it'\\''s/termist' hook --harness claude Stop"
        );
    }

    #[test]
    fn hook_events_map_to_status_signals() {
        let none = json!({});
        assert_eq!(
            signal_for("UserPromptSubmit", &none),
            Some(Signal::PromptSubmitted)
        );
        assert_eq!(
            signal_for("PermissionRequest", &none),
            Some(Signal::NeedsFeedback)
        );
        assert_eq!(signal_for("PostToolUse", &none), Some(Signal::ToolDone));
        assert_eq!(signal_for("Stop", &none), Some(Signal::TurnStopped));
        assert_eq!(signal_for("StopFailure", &none), Some(Signal::TurnStopped));
        assert_eq!(signal_for("SessionStart", &none), None);
        assert_eq!(signal_for("SubagentStop", &none), None);
    }

    #[test]
    fn only_attention_notifications_turn_a_card_red() {
        for t in [
            "permission_prompt",
            "elicitation_dialog",
            "agent_needs_input",
        ] {
            assert_eq!(
                signal_for("Notification", &json!({ "notification_type": t })),
                Some(Signal::NeedsFeedback),
                "{t}"
            );
        }
        for t in ["idle_prompt", "auth_success", "agent_completed"] {
            assert_eq!(
                signal_for("Notification", &json!({ "notification_type": t })),
                None,
                "{t}"
            );
        }
    }

    #[test]
    fn settings_file_is_written_as_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path().to_path_buf());
        paths.ensure().unwrap();
        let p = write_settings(&paths, Path::new("/usr/local/bin/termist")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        assert!(v["hooks"]["Stop"].is_array());
    }
}
