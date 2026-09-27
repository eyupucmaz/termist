//! Listing a folder for the open-project browser. `run` calls it on a blocking
//! thread: a slow, unreadable or huge folder must never freeze the grid.
use std::path::{Path, PathBuf};

/// Folders shown at most; a listing with more says so.
pub const MAX_DIRS: usize = 500;

/// Entries read at most, so a folder with a million files still answers quickly.
pub const MAX_ENTRIES: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    /// `path` with its links resolved, as the daemon stores a project; `path` itself
    /// when that fails.
    pub canonical: PathBuf,
    /// Holds a `.git`: shown with `●`.
    pub git: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<DirEntry>,
    /// Some folders were not read or not shown.
    pub truncated: bool,
}

/// The folders in `dir` (hidden ones left out), sorted by name, git repos marked, links
/// resolved.
pub fn list_dir(dir: &Path) -> Result<Listing, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut dirs = Vec::new();
    let mut truncated = false;
    for (i, entry) in read.enumerate() {
        if i == MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // A link to a folder counts as a folder.
        let is_dir = entry
            .file_type()
            .is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()));
        if is_dir {
            dirs.push((name, path));
        }
    }
    dirs.sort_by_key(|(name, _)| name.to_lowercase());
    if dirs.len() > MAX_DIRS {
        dirs.truncate(MAX_DIRS);
        truncated = true;
    }
    let entries = dirs
        .into_iter()
        .map(|(name, path)| DirEntry {
            git: path.join(".git").exists(),
            canonical: std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
            name,
            path,
        })
        .collect();
    Ok(Listing { entries, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_come_sorted_with_git_repos_marked_and_hidden_ones_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        for d in ["beta", "Alpha/.git", ".hidden", "gamma"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        std::fs::write(tmp.path().join("notes.txt"), "a file, not a folder").unwrap();
        let listing = list_dir(tmp.path()).unwrap();
        let names: Vec<(&str, bool)> = listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.git))
            .collect();
        assert_eq!(names, [("Alpha", true), ("beta", false), ("gamma", false)]);
        assert_eq!(listing.entries[1].path, tmp.path().join("beta"));
        assert!(!listing.truncated);
    }

    // The browser must survive a folder it cannot read and one far too big to show.
    #[test]
    fn a_missing_folder_is_an_error_and_a_huge_one_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let err = list_dir(&tmp.path().join("gone")).unwrap_err();
        assert!(err.starts_with("cannot read"), "{err}");
        for i in 0..MAX_DIRS + 10 {
            std::fs::create_dir(tmp.path().join(format!("d{i:04}"))).unwrap();
        }
        let listing = list_dir(tmp.path()).unwrap();
        assert_eq!(listing.entries.len(), MAX_DIRS);
        assert!(listing.truncated);
        assert_eq!(listing.entries[0].name, "d0000");
    }

    // The daemon opens a project under its real path; the browser must know it to
    // tell whether a link leads to a project that is already open.
    #[cfg(unix)]
    #[test]
    fn a_link_is_shown_as_listed_and_carries_the_real_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let listing = list_dir(&root).unwrap();
        let link = &listing.entries[0];
        assert_eq!(link.name, "link");
        assert_eq!(link.path, root.join("link"));
        assert_eq!(link.canonical, root.join("real"));
        assert_eq!(listing.entries[1].canonical, root.join("real"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = list_dir(&locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err(), "{result:?}");
    }
}
