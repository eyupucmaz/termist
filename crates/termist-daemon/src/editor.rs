//! Which editor opens a folder, or a file at a line, and how: a GUI one in a window of
//! its own, one that runs in a terminal as a card.
use std::path::{Path, PathBuf};

/// Editors with windows of their own; any other runs in a terminal.
pub use termist_core::GUI_EDITORS as GUI;

/// Looked for, in order, when the user named no editor.
pub const FALLBACK: &[&str] = &["code", "cursor", "zed"];

/// What to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// A window of its own: started and left to run, not a card.
    pub gui: bool,
}

/// How `choice` (the user's editor: its program and arguments, as config.toml,
/// `$VISUAL` or `$EDITOR` say it) opens `folder`, or `file` in it at `line`; with no
/// choice, the first of `FALLBACK` found. `find` looks a program up.
pub fn launch(
    choice: Option<&str>,
    folder: &Path,
    file: Option<&str>,
    line: Option<u32>,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<Launch, String> {
    let words: Vec<&str> = choice
        .map(|c| c.split_whitespace().collect())
        .unwrap_or_default();
    // What kind of editor it is goes by the name asked for: a link or a wrapper found
    // in its place is still that editor.
    let (program, rest, asked) = match words.split_first() {
        Some((first, rest)) => {
            let program = find(first).ok_or_else(|| format!("{first} not found"))?;
            let rest = rest.iter().map(|w| w.to_string()).collect();
            (program, rest, first.to_string())
        }
        None => FALLBACK
            .iter()
            .find_map(|name| Some((find(name)?, vec![], name.to_string())))
            .ok_or("no editor found · set editor in config.toml")?,
    };
    let name = Path::new(&asked)
        .file_stem()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let gui = GUI.contains(&name.as_str());
    let mut args: Vec<String> = rest;
    match file {
        None if gui => args.push(folder.display().to_string()),
        None => args.push(".".into()),
        Some(file) => {
            let path = folder.join(file).display().to_string();
            match (name.as_str(), line) {
                (_, None) => args.push(path),
                ("code" | "code-insiders" | "codium" | "cursor" | "windsurf", Some(n)) => {
                    args.extend(["-g".to_string(), format!("{path}:{n}")])
                }
                (
                    "zed" | "subl" | "hx" | "helix" | "idea" | "goland" | "webstorm" | "pycharm"
                    | "rustrover" | "fleet",
                    Some(n),
                ) => args.push(format!("{path}:{n}")),
                (
                    "nvim" | "vim" | "vi" | "emacs" | "emacsclient" | "nano" | "micro" | "kak",
                    Some(n),
                ) => args.extend([format!("+{n}"), path]),
                _ => args.push(path),
            }
        }
    }
    Ok(Launch { program, args, gui })
}

// Paths are written as unix spells them.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn found(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            names
                .contains(&name)
                .then(|| PathBuf::from(format!("/bin/{name}")))
        }
    }

    fn args(l: &Launch) -> Vec<&str> {
        l.args.iter().map(String::as_str).collect()
    }

    #[test]
    fn the_user_s_editor_comes_first_and_opens_the_file_at_its_line() {
        let folder = Path::new("/w/site");
        let all = found(&["nvim", "code", "zed", "hx"]);
        let nvim = launch(Some("nvim"), folder, Some("src/a.rs"), Some(42), &all).unwrap();
        assert_eq!(nvim.program, PathBuf::from("/bin/nvim"));
        assert!(!nvim.gui, "a terminal editor is a card");
        assert_eq!(args(&nvim), ["+42", "/w/site/src/a.rs"]);
        let code = launch(
            Some("code --new-window"),
            folder,
            Some("a.rs"),
            Some(7),
            &all,
        )
        .unwrap();
        assert!(code.gui);
        assert_eq!(args(&code), ["--new-window", "-g", "/w/site/a.rs:7"]);
        let zed = launch(Some("zed"), folder, Some("a.rs"), Some(7), &all).unwrap();
        assert_eq!(args(&zed), ["/w/site/a.rs:7"]);
        let hx = launch(Some("hx"), folder, Some("a.rs"), None, &all).unwrap();
        assert_eq!(args(&hx), ["/w/site/a.rs"], "no line: the file");
        // The folder: a window on it, or the editor started in it.
        assert_eq!(
            args(&launch(Some("code"), folder, None, None, &all).unwrap()),
            ["/w/site"]
        );
        assert_eq!(
            args(&launch(Some("nvim"), folder, None, None, &all).unwrap()),
            ["."]
        );
    }

    #[test]
    fn with_no_choice_code_then_cursor_then_zed_and_otherwise_why_not() {
        let folder = Path::new("/w/site");
        let l = launch(None, folder, None, None, &found(&["zed", "cursor"])).unwrap();
        assert_eq!(l.program, PathBuf::from("/bin/cursor"));
        assert_eq!(
            launch(None, folder, None, None, &found(&[])),
            Err("no editor found · set editor in config.toml".into())
        );
        assert_eq!(
            launch(Some("nvim"), folder, None, None, &found(&[])),
            Err("nvim not found".into())
        );
        assert_eq!(
            launch(Some("  "), folder, None, None, &found(&["code"])).map(|l| l.program),
            Ok(PathBuf::from("/bin/code")),
            "an empty choice is none"
        );
        // An unknown editor gets the file alone, in a terminal.
        let l = launch(Some("ed"), folder, Some("a.rs"), Some(3), &found(&["ed"])).unwrap();
        assert_eq!((args(&l), l.gui), (vec!["/w/site/a.rs"], false));
    }
}
