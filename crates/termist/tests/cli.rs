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

#[test]
fn sound_test_writes_the_sound_and_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let out = termist(tmp.path(), &["sound", "test"]);
    assert!(out.status.success(), "{out:?}");
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    assert_eq!(
        path,
        tmp.path().join("data").join("sounds").join("marti.wav")
    );
    assert!(std::fs::read(&path).unwrap().starts_with(b"RIFF"));
    let out = termist(tmp.path(), &["sound", "test", "kedi"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        tmp.path()
            .join("data")
            .join("sounds")
            .join("kedi.wav")
            .display()
            .to_string()
    );
    let out = termist(tmp.path(), &["sound", "test", "vapur"]);
    assert_eq!(out.status.code(), Some(2), "there is no ferry any more");
}

/// A stand-in `curl` on PATH: GitHub's API names v9.9.9, and the release's installer
/// only says where it was told to install.
#[cfg(unix)]
fn fake_github(dir: &std::path::Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let curl = dir.join("curl");
    std::fs::write(
        &curl,
        "#!/bin/sh\ncase \"$*\" in\n\
         *api.github.com*) echo '[{\"tag_name\":\"v9.9.9\",\"prerelease\":true}]' ;;\n\
         *v9.9.9/termist-installer.sh*) echo 'echo \"installer into $TERMIST_INSTALL_DIR path $TERMIST_NO_MODIFY_PATH\"' ;;\n\
         *) exit 22 ;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::ffi::OsString::from(dir);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    path
}

#[cfg(unix)]
#[test]
fn update_installs_the_newest_release_where_the_installer_put_termist() {
    let tmp = tempfile::tempdir().unwrap();
    let path = fake_github(tmp.path());
    let config = tmp.path().join("xdg");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_termist"))
            .args(args)
            .env("PATH", &path)
            .env("XDG_CONFIG_HOME", &config)
            .env_remove("GITHUB_TOKEN")
            .output()
            .unwrap()
    };
    let out = run(&["update", "--check"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("termist 9.9.9 is out"));

    // No install receipt: this termist was not installed by the installer.
    let out = run(&["update"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("was not put there by termist's installer")
    );

    // A receipt for exactly this binary: the release's installer runs, into its dir.
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_termist"));
    let dir = exe.parent().unwrap();
    std::fs::create_dir_all(config.join("termist")).unwrap();
    std::fs::write(
        config.join("termist").join("termist-receipt.json"),
        format!(
            r#"{{"install_layout":"flat","install_prefix":"{}","version":"0.1.0"}}"#,
            dir.display()
        ),
    )
    .unwrap();
    let out = run(&["upgrade"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout.contains("updating"), "{stdout}");
    assert!(
        stdout.contains(&format!("installer into {} path 1", dir.display())),
        "{stdout}"
    );
    assert!(stdout.contains("termist kill"), "{stdout}");
}
