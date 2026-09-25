use std::process::Command;

#[test]
fn prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_termist"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "termist 0.1.0");
}

// A malformed `termist hook …` must not exit 2: Claude Code treats that as "block".
#[test]
fn a_malformed_hook_command_line_still_exits_zero_silently() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        &["hook"][..],
        &["hook", "--harness", "claude"],
        &["hook", "--bogus", "Stop"],
        &["hook", "--harness", "claude", "Stop", "extra"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_termist"))
            .args(args)
            .env("TERMIST_HOME", tmp.path())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn other_usage_errors_still_report_and_fail() {
    let out = Command::new(env!("CARGO_BIN_EXE_termist"))
        .arg("no-such-command")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-command"));
}
