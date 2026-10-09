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

/// Writes the plugin, and beside it the words agents are told of termist (OpenCode
/// reads instructions from a file).
pub fn write_plugin(config_dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = config_dir.join("plugins");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("termist.ts");
    std::fs::write(&path, PLUGIN_TS)?;
    std::fs::write(teach_file(config_dir), termist_core::agents::TEACH)?;
    Ok(path)
}

/// Where `write_plugin` puts termist's words for agents.
pub fn teach_file(config_dir: &Path) -> PathBuf {
    config_dir.join("termist-agents.md")
}

/// What a card adds to OpenCode's config: termist's words as instructions, and leave
/// to work in `also` (its repo's worktrees) without asking. `None`: nothing.
pub fn extra(config_dir: &Path, teach: bool, also: Option<&Path>) -> Option<Value> {
    let mut extra = serde_json::Map::new();
    if teach {
        extra.insert(
            "instructions".into(),
            json!([teach_file(config_dir).display().to_string()]),
        );
    }
    if let Some(dir) = also {
        let pattern = format!("{}/**", dir.display().to_string().replace('\\', "/"));
        extra.insert(
            "permission".into(),
            json!({ "external_directory": { pattern: "allow" } }),
        );
    }
    (!extra.is_empty()).then_some(Value::Object(extra))
}

/// `extra` laid over `content`: its instructions after the user's, its folders among
/// theirs. `None` when `content` is not the shape OpenCode reads.
fn with_extra(mut content: Value, extra: Option<Value>) -> Option<Value> {
    let Some(Value::Object(extra)) = extra else {
        return Some(content);
    };
    let config = content.as_object_mut()?;
    if let Some(Value::Array(more)) = extra.get("instructions") {
        let list = config.entry("instructions").or_insert(json!([]));
        if list.is_null() {
            *list = json!([]);
        }
        list.as_array_mut()?.extend(more.iter().cloned());
    }
    if let Some(Value::Object(dirs)) = extra
        .get("permission")
        .and_then(|p| p.get("external_directory"))
    {
        let permission = config.entry("permission").or_insert(json!({}));
        let permission = permission.as_object_mut()?;
        let external = permission.entry("external_directory").or_insert(json!({}));
        // A single word ("ask") for every folder becomes the rule for the others.
        if let Value::String(all) = external {
            *external = json!({ "*": all.clone() });
        }
        let external = external.as_object_mut()?;
        for (dir, rule) in dirs {
            external.insert(dir.clone(), rule.clone());
        }
    }
    Some(content)
}

/// A `file://` URL for `path`, percent-encoded as an RFC 3986 path: a space or a `#`
/// in the data dir must not end the path. A Windows drive path gets its own `/`, a UNC
/// path (`\\server\share`) names its server as the host, and the verbatim `\\?\`
/// prefix is dropped.
fn file_url(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    let s = match s.strip_prefix("//?/") {
        Some(rest) => match rest.strip_prefix("UNC/") {
            Some(unc) => format!("//{unc}"),
            None => rest.to_string(),
        },
        None => s,
    };
    let mut url = String::from(if s.starts_with("//") {
        "file:"
    } else if s.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// Spawn env for an OpenCode session. The user's own `OPENCODE_CONFIG_DIR` wins; then
/// our plugin is added through `OPENCODE_CONFIG_CONTENT` instead (untested upstream,
/// so logged), merged into the user's own `OPENCODE_CONFIG_CONTENT` if they have one.
/// `extra` (see `extra`) always goes through `OPENCODE_CONFIG_CONTENT`, over theirs.
pub fn config_env(
    config_dir: &Path,
    users_dir: Option<&OsStr>,
    users_content: Option<&OsStr>,
    extra: Option<Value>,
) -> Vec<(String, String)> {
    // An empty value configures nothing: as if it were unset.
    let users = users_content.filter(|c| !c.to_string_lossy().trim().is_empty());
    if users_dir.is_none() {
        let mut env = vec![(
            "OPENCODE_CONFIG_DIR".into(),
            config_dir.display().to_string(),
        )];
        if extra.is_none() {
            return env;
        }
        let content = match users {
            None => Some(json!({})),
            Some(users) => users
                .to_str()
                .and_then(|u| serde_json::from_str::<Value>(u).ok()),
        };
        match content.and_then(|c| with_extra(c, extra)) {
            Some(content) => env.push(("OPENCODE_CONFIG_CONTENT".into(), content.to_string())),
            None => tracing::warn!(
                "could not read OPENCODE_CONFIG_CONTENT as a JSON object; it is passed on \
                 unchanged, without termist's words for the agent"
            ),
        }
        return env;
    }
    tracing::warn!(
        "OPENCODE_CONFIG_DIR is set; adding the termist plugin through OPENCODE_CONFIG_CONTENT"
    );
    let plugin = file_url(&config_dir.join("plugins").join("termist.ts"));
    let content = match users {
        None => Some(json!({ "plugin": [plugin] })),
        Some(users) => with_plugin(users, plugin),
    };
    match content.and_then(|c| with_extra(c, extra)) {
        Some(content) => vec![("OPENCODE_CONFIG_CONTENT".into(), content.to_string())],
        None => {
            tracing::warn!(
                "could not read OPENCODE_CONFIG_CONTENT as a JSON object with a plugin list; \
                 it is passed on unchanged, without the termist plugin (no status for OpenCode)"
            );
            vec![]
        }
    }
}

/// The user's config content with our plugin added to its `plugin` list, once. `None`
/// when it is not plain JSON of that shape (OpenCode also takes JSONC, which we don't
/// rewrite).
fn with_plugin(users: &OsStr, plugin: String) -> Option<Value> {
    let mut config: Value = serde_json::from_str(users.to_str()?).ok()?;
    let list = config
        .as_object_mut()?
        .entry("plugin")
        .or_insert(Value::Null);
    if list.is_null() {
        *list = json!([]);
    }
    let list = list.as_array_mut()?;
    if !list.iter().any(|p| p.as_str() == Some(plugin.as_str())) {
        list.push(Value::String(plugin));
    }
    Some(config)
}

/// `opencode [--session <id>] [-m <provider/model>] [--prompt=<text>]`. OpenCode has
/// no effort flag; a resumed session gets no prompt. The prompt rides in the same
/// argument as its flag, so a text starting with `-` is never read as a flag.
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
        args.push(format!("--prompt={p}"));
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
    fn the_words_for_agents_are_written_beside_the_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(teach_file(tmp.path())).unwrap(),
            termist_core::agents::TEACH
        );
    }

    #[test]
    fn termist_s_words_and_folder_join_the_user_s_own_config_content() {
        let dir = std::path::Path::new("/data/opencode");
        let extra = || extra(dir, true, Some(std::path::Path::new("/w/site-worktrees")));
        let words = "/data/opencode/termist-agents.md";
        // Ours alone, beside our config dir.
        let env = config_env(dir, None, None, extra());
        assert_eq!(env[0].0, "OPENCODE_CONFIG_DIR");
        let v: serde_json::Value = serde_json::from_str(&env[1].1).unwrap();
        assert_eq!(
            v,
            json!({"instructions": [words],
                   "permission": {"external_directory": {"/w/site-worktrees/**": "allow"}}})
        );
        // Theirs kept: their instructions first, their folder rules beside ours.
        let users = r#"{"instructions":["AGENTS.md"],"permission":{"external_directory":{"/notes/**":"allow"}}}"#;
        let env = config_env(dir, None, Some(OsStr::new(users)), extra());
        let v: serde_json::Value = serde_json::from_str(&env[1].1).unwrap();
        assert_eq!(v["instructions"], json!(["AGENTS.md", words]));
        assert_eq!(
            v["permission"]["external_directory"],
            json!({"/notes/**": "allow", "/w/site-worktrees/**": "allow"})
        );
        // One word for every folder stays the rule for the others.
        let users = r#"{"permission":{"external_directory":"deny"}}"#;
        let env = config_env(dir, None, Some(OsStr::new(users)), extra());
        let v: serde_json::Value = serde_json::from_str(&env[1].1).unwrap();
        assert_eq!(
            v["permission"]["external_directory"],
            json!({"*": "deny", "/w/site-worktrees/**": "allow"})
        );
        // Nothing to add: nothing but our config dir.
        assert_eq!(extra_none(dir).len(), 1);
    }

    fn extra_none(dir: &std::path::Path) -> Vec<(String, String)> {
        config_env(dir, None, None, extra(dir, false, None))
    }

    #[test]
    fn our_config_dir_is_used_unless_the_user_has_their_own() {
        let dir = std::path::Path::new("/data/opencode");
        assert_eq!(
            config_env(dir, None, None, None),
            vec![(
                "OPENCODE_CONFIG_DIR".to_string(),
                "/data/opencode".to_string()
            )]
        );
        let env = config_env(dir, Some(OsStr::new("/home/me/.oc")), None, None);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "OPENCODE_CONFIG_CONTENT");
        let v: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(v["plugin"], json!([PLUGIN_URL]));
    }

    const PLUGIN_URL: &str = "file:///data/opencode/plugins/termist.ts";

    fn content_with(users: &str) -> Option<serde_json::Value> {
        let env = config_env(
            std::path::Path::new("/data/opencode"),
            Some(OsStr::new("/home/me/.oc")),
            Some(OsStr::new(users)),
            None,
        );
        let (key, value) = env.into_iter().next()?;
        assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
        Some(serde_json::from_str(&value).unwrap())
    }

    // The user's own inline config stays; our plugin joins theirs.
    #[test]
    fn our_plugin_is_added_to_the_users_config_content() {
        assert_eq!(
            content_with(r#"{"model":"a/b","plugin":["their-plugin"]}"#),
            Some(json!({"model":"a/b","plugin":["their-plugin", PLUGIN_URL]}))
        );
        assert_eq!(
            content_with(r#"{"model":"a/b"}"#),
            Some(json!({"model":"a/b","plugin":[PLUGIN_URL]}))
        );
        assert_eq!(
            content_with(&format!(r#"{{"plugin":["{PLUGIN_URL}"]}}"#)),
            Some(json!({"plugin":[PLUGIN_URL]})),
            "not twice"
        );
    }

    // An empty value configures nothing, and a null plugin list is no list.
    #[test]
    fn an_empty_config_content_or_a_null_plugin_list_is_no_obstacle() {
        for users in ["", "  \n"] {
            assert_eq!(
                content_with(users),
                Some(json!({"plugin":[PLUGIN_URL]})),
                "{users:?}"
            );
        }
        assert_eq!(
            content_with(r#"{"model":"a/b","plugin":null}"#),
            Some(json!({"model":"a/b","plugin":[PLUGIN_URL]}))
        );
    }

    // No merge is better than a broken one: their value is left as it is, without us.
    #[test]
    fn a_config_content_we_cannot_read_is_left_alone() {
        for users in [
            "{ // a comment\n \"model\": \"a/b\" }",
            "[1]",
            r#"{"plugin":"one"}"#,
        ] {
            assert_eq!(content_with(users), None, "{users}");
        }
    }

    #[test]
    fn plugin_paths_are_proper_file_urls() {
        assert_eq!(
            file_url(std::path::Path::new("/Users/me/My Data/#1/termist.ts")),
            "file:///Users/me/My%20Data/%231/termist.ts"
        );
        assert_eq!(
            file_url(std::path::Path::new(r"C:\Users\Me Too\AppData\termist.ts")),
            "file:///C:/Users/Me%20Too/AppData/termist.ts"
        );
        assert_eq!(
            file_url(std::path::Path::new("/tmp/ü?%.ts")),
            "file:///tmp/%C3%BC%3F%25.ts"
        );
    }

    // A data dir on a network share, or given in Windows' verbatim `\\?\` form.
    #[test]
    fn unc_and_verbatim_windows_paths_become_file_urls() {
        let url = |p: &str| file_url(std::path::Path::new(p));
        assert_eq!(
            url(r"\\server\share\My Data\termist.ts"),
            "file://server/share/My%20Data/termist.ts"
        );
        assert_eq!(
            url(r"\\?\C:\Users\Me\termist.ts"),
            "file:///C:/Users/Me/termist.ts"
        );
        assert_eq!(
            url(r"\\?\UNC\server\share\termist.ts"),
            "file://server/share/termist.ts"
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
            ["-m", "anthropic/claude-sonnet-4-5", "--prompt=fix it"]
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
