//! The models each agent CLI offers, for the quick prompt. Asked of the CLI itself, so
//! the list does not go stale: `codex debug models`, `opencode models`. Claude Code
//! lists none; its aliases always mean the newest model of their kind.
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use termist_core::{Harness, ModelInfo};

pub const CLAUDE_ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];
/// How long a CLI may take to list its models.
pub const LIMIT: Duration = Duration::from_secs(10);

pub fn claude() -> Vec<ModelInfo> {
    CLAUDE_ALIASES
        .iter()
        .map(|a| ModelInfo {
            id: a.to_string(),
            label: a.to_string(),
            efforts: vec![],
        })
        .collect()
}

/// `codex debug models` prints `{"models": [{"slug", "display_name", "visibility",
/// "supported_reasoning_levels": [{"effort"}]}]}`; the ones it lists are `"list"`.
pub fn parse_codex(json: &str) -> Vec<ModelInfo> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    let Some(models) = value.get("models").and_then(|m| m.as_array()) else {
        return vec![];
    };
    models
        .iter()
        .filter(|m| m.get("visibility").and_then(|v| v.as_str()) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?.to_string();
            let label = m
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            let efforts = m
                .get("supported_reasoning_levels")
                .and_then(|l| l.as_array())
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("effort")?.as_str().map(String::from))
                .collect();
            Some(ModelInfo { id, label, efforts })
        })
        .collect()
}

/// `opencode models` prints one `provider/model` a line.
pub fn parse_opencode(text: &str) -> Vec<ModelInfo> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(char::is_whitespace))
        .map(|l| ModelInfo {
            id: l.into(),
            label: l.into(),
            efforts: vec![],
        })
        .collect()
}

/// Runs `program args` and returns what it printed if it succeeds within `limit`; one
/// that takes longer is killed.
pub fn run(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read while it runs, so a long list cannot fill the pipe and stall it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Something the CLI started may keep the pipe open after it exits, so the
                // output is waited for only until the deadline.
                let out = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                // The reader ends when the pipe closes; it is not waited for, in case
                // something the CLI started keeps the pipe open.
                return None;
            }
        }
    }
}

/// What `harness`'s CLI at `program` offers; empty when it cannot say.
pub fn catalog(harness: Harness, program: &str) -> Vec<ModelInfo> {
    match harness {
        Harness::Claude => claude(),
        Harness::Codex => run(program, &["debug", "models"], LIMIT)
            .map(|out| parse_codex(&out))
            .unwrap_or_default(),
        Harness::OpenCode => run(program, &["models"], LIMIT)
            .map(|out| parse_opencode(&out))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, label: &str, efforts: &[&str]) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            label: label.into(),
            efforts: efforts.iter().map(|e| e.to_string()).collect(),
        }
    }

    #[test]
    fn claude_offers_its_aliases() {
        let ids: Vec<String> = claude().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["opus", "sonnet", "haiku", "fable"]);
    }

    // Shaped like `codex debug models` from codex-cli 0.156.1.
    #[test]
    fn codex_lists_what_it_shows_with_their_efforts() {
        let json = r#"{"models":[
          {"slug":"gpt-6-astra","display_name":"GPT-6-Astra","visibility":"list",
           "supported_reasoning_levels":[{"effort":"low","description":"x"},{"effort":"ultra","description":"y"}]},
          {"slug":"gpt-reserve","display_name":"GPT-Reserve","visibility":"hide",
           "supported_reasoning_levels":[{"effort":"low"}]},
          {"slug":"gpt-5.5","visibility":"list","supported_reasoning_levels":[]}
        ]}"#;
        assert_eq!(
            parse_codex(json),
            [
                info("gpt-6-astra", "GPT-6-Astra", &["low", "ultra"]),
                info("gpt-5.5", "gpt-5.5", &[]),
            ]
        );
    }

    #[test]
    fn codex_output_that_is_not_its_json_lists_nothing() {
        assert!(parse_codex("not json").is_empty());
        assert!(parse_codex(r#"{"models": 3}"#).is_empty());
        assert!(
            parse_codex(r#"{"models": [{"display_name": "no slug", "visibility": "list"}]}"#)
                .is_empty()
        );
    }

    #[test]
    fn opencode_lists_provider_slash_model_lines() {
        let text =
            "opencode/big-pickle\n\ndeepseek/deepseek-v4-pro\nWarning: a new version is out\n";
        assert_eq!(
            parse_opencode(text),
            [
                info("opencode/big-pickle", "opencode/big-pickle", &[]),
                info("deepseek/deepseek-v4-pro", "deepseek/deepseek-v4-pro", &[]),
            ]
        );
    }

    #[cfg(unix)]
    fn script(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("cli.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_answers_is_read_and_one_that_fails_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = script(tmp.path(), "echo \"$1 $2\"");
        assert_eq!(
            run(&ok, &["debug", "models"], LIMIT).as_deref(),
            Some("debug models\n")
        );
        let failing = script(tmp.path(), "echo half; exit 1");
        assert_eq!(run(&failing, &[], LIMIT), None);
        assert_eq!(run("/no/such/cli", &[], LIMIT), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_hangs_is_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let hangs = script(tmp.path(), "sleep 30");
        let started = Instant::now();
        assert_eq!(run(&hangs, &[], Duration::from_millis(300)), None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_that_exits_but_leaves_its_output_open_is_not_waited_for() {
        let tmp = tempfile::tempdir().unwrap();
        // The background `sleep` inherits stdout and keeps the pipe open.
        let lingers = script(tmp.path(), "sleep 30 &\necho done");
        let started = Instant::now();
        assert_eq!(run(&lingers, &[], Duration::from_millis(300)), None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }
}
