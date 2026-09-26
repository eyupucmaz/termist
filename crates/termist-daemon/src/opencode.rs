//! OpenCode status through a plugin that lives in termist's own config dir, which
//! OpenCode layers on top of the user's config.
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use termist_core::Signal;

/// Forwards OpenCode bus events to `termist hook`, one at a time and in order, and
/// drops events from child (subagent) sessions.
pub const PLUGIN_TS: &str = r#"// Written by termist; it is rewritten every time the termist daemon starts.
// Forwards OpenCode status events to the termist daemon so the session card can show them.
const BIN = process.env.TERMIST_BIN
const FORWARD = new Set([
  "session.created", "session.status", "session.idle", "session.error",
  "permission.asked", "permission.replied", "question.asked", "question.replied", "question.rejected",
])
const children = new Set<string>()
let chain: Promise<unknown> = Promise.resolve()

function forward(name: string, payload: unknown) {
  if (!BIN || !process.env.TERMIST_SESSION_ID) return
  chain = chain
    .then(async () => {
      const proc = Bun.spawn([BIN, "hook", "--harness", "opencode", name], {
        stdin: "pipe", stdout: "ignore", stderr: "ignore",
      })
      proc.stdin.write(JSON.stringify(payload))
      proc.stdin.end()
      await proc.exited
    })
    .catch(() => {})
}

function sessionOf(event: any): string | undefined {
  return event?.properties?.sessionID ?? event?.properties?.info?.id
}

export const TermistPlugin = async () => ({
  event: async ({ event }: any) => {
    const type = event?.type
    if (type === "session.created" && event?.properties?.info?.parentID) {
      const id = sessionOf(event)
      if (id) children.add(id)
      return
    }
    if (!FORWARD.has(type)) return
    const id = sessionOf(event)
    if (id && children.has(id)) return
    forward(type, event)
  },
  "chat.message": async (input: any) => {
    if (input?.sessionID && children.has(input.sessionID)) return
    forward("chat.message", input)
  },
})
"#;

pub fn write_plugin(config_dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = config_dir.join("plugins");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("termist.ts");
    std::fs::write(&path, PLUGIN_TS)?;
    Ok(path)
}

fn file_url(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// Spawn env for an OpenCode session. The user's own `OPENCODE_CONFIG_DIR` wins; then
/// our plugin is added through `OPENCODE_CONFIG_CONTENT` instead (untested upstream, so logged).
pub fn config_env(config_dir: &Path, users_dir: Option<&OsStr>) -> Vec<(String, String)> {
    match users_dir {
        None => vec![(
            "OPENCODE_CONFIG_DIR".into(),
            config_dir.display().to_string(),
        )],
        Some(_) => {
            tracing::warn!(
                "OPENCODE_CONFIG_DIR is set; adding the termist plugin through OPENCODE_CONFIG_CONTENT"
            );
            let plugin = file_url(&config_dir.join("plugins").join("termist.ts"));
            vec![(
                "OPENCODE_CONFIG_CONTENT".into(),
                json!({ "plugin": [plugin] }).to_string(),
            )]
        }
    }
}

/// `opencode [--session <id>] [-m <provider/model>] [--prompt <text>]`. OpenCode has
/// no effort flag; a resumed session gets no prompt.
pub fn args(resume: Option<&str>, model: Option<&str>, prompt: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(id) = resume {
        args.extend(["--session".to_string(), id.to_string()]);
    }
    if let Some(m) = model {
        args.extend(["-m".to_string(), m.to_string()]);
    }
    if resume.is_none()
        && let Some(p) = prompt.filter(|p| !p.trim().is_empty())
    {
        args.extend(["--prompt".to_string(), p.to_string()]);
    }
    args
}

pub fn event_session(payload: &Value) -> Option<&str> {
    ["/properties/sessionID", "/properties/info/id", "/sessionID"]
        .iter()
        .find_map(|p| payload.pointer(p).and_then(Value::as_str))
}

pub fn is_child_session(payload: &Value) -> bool {
    payload
        .pointer("/properties/info/parentID")
        .and_then(Value::as_str)
        .is_some()
}

pub fn signal_for(event: &str, payload: &Value) -> Option<Signal> {
    let at = |p: &str| payload.pointer(p).and_then(Value::as_str);
    match event {
        "chat.message" => Some(Signal::PromptSubmitted),
        "session.status" => match at("/properties/status/type") {
            Some("busy") => Some(Signal::PromptSubmitted),
            Some("idle") => Some(Signal::TurnStopped),
            _ => None,
        },
        "session.idle" => Some(Signal::TurnStopped),
        "session.error" => match at("/properties/error/name") {
            Some("MessageAbortedError") => Some(Signal::Cancelled),
            _ => Some(Signal::TurnStopped),
        },
        "permission.asked" | "question.asked" => Some(Signal::NeedsFeedback),
        "permission.replied" => match at("/properties/reply") {
            Some("reject") => None,
            _ => Some(Signal::ToolDone),
        },
        "question.replied" | "question.rejected" => Some(Signal::ToolDone),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsStr;

    #[test]
    fn the_plugin_is_written_into_our_config_dir_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_plugin(tmp.path()).unwrap();
        assert_eq!(path, tmp.path().join("plugins").join("termist.ts"));
        let src = std::fs::read_to_string(&path).unwrap();
        assert_eq!(src, PLUGIN_TS);
        // the properties the daemon relies on
        assert!(src.contains("--harness\", \"opencode\""));
        assert!(
            src.contains("await proc.exited"),
            "spawns must be serialized"
        );
        assert!(src.contains("parentID"), "child sessions must be dropped");
    }

    #[test]
    fn our_config_dir_is_used_unless_the_user_has_their_own() {
        let dir = std::path::Path::new("/data/opencode");
        assert_eq!(
            config_env(dir, None),
            vec![(
                "OPENCODE_CONFIG_DIR".to_string(),
                "/data/opencode".to_string()
            )]
        );
        let env = config_env(dir, Some(OsStr::new("/home/me/.oc")));
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "OPENCODE_CONFIG_CONTENT");
        let v: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert!(
            v["plugin"][0]
                .as_str()
                .unwrap()
                .ends_with("/data/opencode/plugins/termist.ts")
        );
    }

    #[test]
    fn resume_uses_session_flag() {
        assert_eq!(
            args(Some("ses_1"), None, Some("ignored")),
            vec!["--session".to_string(), "ses_1".to_string()]
        );
        assert!(args(None, None, None).is_empty());
        assert!(
            args(None, None, Some("  ")).is_empty(),
            "a blank prompt is no prompt"
        );
    }

    #[test]
    fn the_model_and_the_prompt_are_flags() {
        assert_eq!(
            args(None, Some("anthropic/claude-sonnet-4-5"), Some("fix it")),
            ["-m", "anthropic/claude-sonnet-4-5", "--prompt", "fix it"]
        );
        assert_eq!(
            args(Some("ses_1"), Some("openai/gpt-5"), None),
            ["--session", "ses_1", "-m", "openai/gpt-5"]
        );
    }

    #[test]
    fn opencode_events_map_to_signals() {
        let s = |e: &str, p: serde_json::Value| signal_for(e, &p);
        assert_eq!(s("chat.message", json!({})), Some(Signal::PromptSubmitted));
        assert_eq!(
            s(
                "session.status",
                json!({"properties":{"status":{"type":"busy"}}})
            ),
            Some(Signal::PromptSubmitted)
        );
        assert_eq!(
            s(
                "session.status",
                json!({"properties":{"status":{"type":"idle"}}})
            ),
            Some(Signal::TurnStopped)
        );
        assert_eq!(
            s(
                "session.status",
                json!({"properties":{"status":{"type":"retry"}}})
            ),
            None
        );
        assert_eq!(s("session.idle", json!({})), Some(Signal::TurnStopped));
        assert_eq!(
            s(
                "session.error",
                json!({"properties":{"error":{"name":"MessageAbortedError"}}})
            ),
            Some(Signal::Cancelled)
        );
        assert_eq!(
            s(
                "session.error",
                json!({"properties":{"error":{"name":"APIError"}}})
            ),
            Some(Signal::TurnStopped)
        );
        assert_eq!(
            s("permission.asked", json!({})),
            Some(Signal::NeedsFeedback)
        );
        assert_eq!(s("question.asked", json!({})), Some(Signal::NeedsFeedback));
        assert_eq!(
            s("permission.replied", json!({"properties":{"reply":"once"}})),
            Some(Signal::ToolDone)
        );
        assert_eq!(
            s(
                "permission.replied",
                json!({"properties":{"reply":"reject"}})
            ),
            None
        );
        assert_eq!(s("question.rejected", json!({})), Some(Signal::ToolDone));
        assert_eq!(s("session.created", json!({})), None);
    }

    #[test]
    fn session_ids_and_children_are_read_from_event_payloads() {
        assert_eq!(
            event_session(&json!({"properties":{"sessionID":"ses_a"}})),
            Some("ses_a")
        );
        assert_eq!(
            event_session(&json!({"properties":{"info":{"id":"ses_b"}}})),
            Some("ses_b")
        );
        assert_eq!(
            event_session(&json!({"sessionID":"ses_c"})),
            Some("ses_c"),
            "chat.message input"
        );
        assert!(is_child_session(
            &json!({"properties":{"info":{"id":"ses_d","parentID":"ses_a"}}})
        ));
        assert!(!is_child_session(
            &json!({"properties":{"info":{"id":"ses_a"}}})
        ));
    }
}
