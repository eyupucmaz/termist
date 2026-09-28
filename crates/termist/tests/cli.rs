use std::process::Command;

#[test]
fn prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_termist"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        concat!("termist ", env!("CARGO_PKG_VERSION"))
    );
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

fn termist(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_termist"))
        .args(args)
        .env("TERMIST_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn config_path_check_export_and_import() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let out = termist(home, &["config", "path"]);
    assert!(out.status.success());
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    assert_eq!(path, home.join("config").join("config.toml"));

    assert!(
        termist(home, &["config", "check"]).status.success(),
        "no file is fine"
    );

    let bad = home.join("bad.toml");
    std::fs::write(&bad, "theme = \"nope\"\n").unwrap();
    let out = termist(home, &["config", "import", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown theme \"nope\""));
    assert!(!path.exists(), "a file with problems is not imported");

    let good = home.join("good.toml");
    std::fs::write(&good, "# mine\ntheme = \"moda\"\n").unwrap();
    assert!(
        termist(home, &["config", "import", good.to_str().unwrap()])
            .status
            .success()
    );
    let out = termist(home, &["config", "export"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "# mine\ntheme = \"moda\"\n"
    );

    let bad_keys = home.join("keys.toml");
    std::fs::write(&bad_keys, "prefix = \"x\"\n[keys.grid]\ng = \"fly\"\n").unwrap();
    let out = termist(home, &["config", "import", bad_keys.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "keys are checked too");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("prefix: x would no longer reach the session"),
        "{err}"
    );
    assert!(
        err.contains("keys.grid.g: \"fly\" is not a grid action"),
        "{err}"
    );

    std::fs::write(
        &path,
        "theme = \"moda\"\nmystery = 1\n[keys.focus]\n\"C-q\" = \"help\"\n",
    )
    .unwrap();
    let out = termist(home, &["config", "check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("mystery: unknown setting"), "{err}");
    assert!(
        err.contains("keys.focus.C-q: C-q always gets you out"),
        "{err}"
    );
}
