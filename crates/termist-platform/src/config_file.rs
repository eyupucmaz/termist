//! Reading and replacing the config files on disk.
use crate::Paths;
use std::io;
use std::path::Path;
use termist_core::config::{Config, Problem};

/// The config in force: `config.toml` with `config.local.toml` over it. A missing file
/// is an empty one; an unreadable one is a problem, like a bad value.
pub fn load(paths: &Paths) -> (Config, Vec<Problem>) {
    let mut problems = Vec::new();
    let main = read(&paths.config_path(), &mut problems).unwrap_or_default();
    let local = read(&paths.config_local_path(), &mut problems);
    let (config, mut more) = Config::parse(&main, local.as_deref());
    problems.append(&mut more);
    (config, problems)
}

fn read(path: &Path, problems: &mut Vec<Problem>) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            problems.push(Problem {
                path: path.display().to_string(),
                message: format!("cannot be read: {e}"),
            });
            None
        }
    }
}

/// Replaces `config.toml` with `text` if it has no problems, keeping the old file as
/// `config.toml.bak`. Returns the problems instead when there are any.
pub fn import(paths: &Paths, text: &str) -> io::Result<Result<(), Vec<Problem>>> {
    let (_, problems) = Config::parse(text, None);
    if !problems.is_empty() {
        return Ok(Err(problems));
    }
    write(paths, text)?;
    Ok(Ok(()))
}

/// Writes `config.toml` through a temporary file, keeping the old one as
/// `config.toml.bak`.
pub fn write(paths: &Paths, text: &str) -> io::Result<()> {
    std::fs::create_dir_all(&paths.config_dir)?;
    let path = paths.config_path();
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    if path.exists() {
        std::fs::copy(&path, path.with_extension("toml.bak"))?;
    }
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(tmp: &tempfile::TempDir) -> Paths {
        Paths::under(tmp.path().to_path_buf())
    }

    #[test]
    fn no_files_is_the_default_config() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(&paths(&tmp)), (Config::default(), vec![]));
    }

    #[test]
    fn the_local_file_is_layered_over_the_main_one() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(&p.config_dir).unwrap();
        std::fs::write(p.config_path(), "theme = \"moda\"\nprefix = \"C-b\"\n").unwrap();
        std::fs::write(p.config_local_path(), "theme = \"terminal\"\n").unwrap();
        let (config, problems) = load(&p);
        assert!(problems.is_empty());
        assert_eq!(config.theme, "terminal");
        assert_eq!(config.prefix, "C-b");
    }

    #[test]
    fn import_refuses_a_file_with_problems_and_keeps_the_old_one_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&tmp);
        write(&p, "theme = \"moda\"\n").unwrap();
        let refused = import(&p, "theme = \"nope\"\n").unwrap().unwrap_err();
        assert_eq!(refused[0].path, "theme");
        assert_eq!(load(&p).0.theme, "moda", "untouched");
        import(&p, "theme = \"terminal\"\n").unwrap().unwrap();
        assert_eq!(load(&p).0.theme, "terminal");
        let bak = std::fs::read_to_string(p.config_path().with_extension("toml.bak")).unwrap();
        assert_eq!(bak, "theme = \"moda\"\n");
    }
}
