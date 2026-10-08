//! Ayna in the registry: who looks at which folder's diff, reading it on a blocking
//! thread, and looking again every two seconds while someone looks.
use super::*;
use crate::mirror;
use std::time::Instant;
use termist_core::github::Viewed;
use termist_core::{DiffMode, LocalDiffData, ReadState};

/// How often a folder someone looks at is checked for changes.
pub const LOOK_EVERY: Duration = Duration::from_secs(2);

/// A folder's diff in one mode, while someone looks at it.
pub type Key = (PathBuf, DiffMode);

#[derive(Debug, Default)]
pub struct Looked {
    pub lookers: HashSet<ClientId>,
    pub reading: bool,
    /// Asked again while a read was on its way.
    pub again: bool,
    pub checking: bool,
    pub checked: Option<Instant>,
    /// The fingerprint the last read saw; `Some(None)`: git could not say.
    pub print: Option<Option<u64>>,
    /// The repo's root, once a read found it.
    pub root: Option<PathBuf>,
    pub last: Option<Box<LocalDiffData>>,
}

/// What a blocking job of Ayna's found.
#[derive(Debug)]
pub enum Mirrored {
    Read {
        key: Key,
        print: Option<u64>,
        result: Result<mirror::Read, String>,
    },
    Checked {
        key: Key,
        print: Option<u64>,
    },
}

impl Registry {
    /// `client` looks at the diff of `path` (or at none): read at once, the last one
    /// read sent meanwhile.
    pub(super) fn set_local_diff(
        &mut self,
        client: ClientId,
        path: Option<PathBuf>,
        mode: DiffMode,
    ) {
        let key = path.clone().map(|p| (p, mode));
        // Asked again (`R`): it still looks, and what was read stays meanwhile.
        if key.is_none() || self.looking.get(&client) != key.as_ref() {
            self.stop_looking(client);
        }
        let (Some(path), Some(key)) = (path, key) else {
            return;
        };
        self.looking.insert(client, key.clone());
        let looked = self.looked.entry(key.clone()).or_default();
        looked.lookers.insert(client);
        let diff = looked.last.clone();
        self.send(
            client,
            ServerEvent::LocalDiff {
                path,
                mode,
                state: ReadState::Reading,
                diff,
            },
        );
        self.read_local(key);
    }

    /// `client` looks no longer; a folder nobody looks at is not watched.
    pub(super) fn stop_looking(&mut self, client: ClientId) {
        let Some(key) = self.looking.remove(&client) else {
            return;
        };
        if let Some(looked) = self.looked.get_mut(&key) {
            looked.lookers.remove(&client);
            if looked.lookers.is_empty() {
                self.looked.remove(&key);
            }
        }
    }

    /// The ref a kept worktree's branch is measured from, for a folder in it.
    fn kept_base(&self, path: &Path) -> Option<String> {
        let path = place::resolved(path);
        self.store
            .worktrees()
            .unwrap_or_default()
            .into_iter()
            .find(|w| path.starts_with(place::resolved(&w.path)))
            .and_then(|w| w.base)
    }

    /// Reads the diff on a blocking thread; one already on its way reads again after.
    fn read_local(&mut self, key: Key) {
        let base = self.kept_base(&key.0);
        let Some(looked) = self.looked.get_mut(&key) else {
            return;
        };
        if looked.reading {
            looked.again = true;
            return;
        }
        looked.reading = true;
        let (tx, root) = (self.mirror_tx.clone(), looked.root.clone());
        tokio::task::spawn_blocking(move || {
            let git = crate::github::worktree::git_raw;
            let job = std::panic::catch_unwind(|| {
                // Seen before reading: a change made while it reads is read next time.
                let before = root.as_ref().and_then(|r| mirror::fingerprint(r, &git));
                let result = mirror::read(&key.0, key.1, base.as_deref(), &git);
                let print = before.or_else(|| {
                    let root = result.as_ref().ok()?.root.clone();
                    mirror::fingerprint(&root, &git)
                });
                (print, result)
            });
            let (print, result) =
                job.unwrap_or_else(|_| (None, Err("reading the diff failed".into())));
            let _ = tx.send(Mirrored::Read { key, print, result });
        });
    }

    pub fn mirrored(&mut self, done: Mirrored, now: Instant) {
        match done {
            Mirrored::Read { key, print, result } => self.local_read(key, print, result, now),
            Mirrored::Checked { key, print } => {
                let Some(looked) = self.looked.get_mut(&key) else {
                    return;
                };
                looked.checking = false;
                looked.checked = Some(now);
                if looked.print != Some(print) {
                    self.read_local(key);
                }
            }
        }
    }

    fn local_read(
        &mut self,
        key: Key,
        print: Option<u64>,
        result: Result<mirror::Read, String>,
        now: Instant,
    ) {
        let Some(looked) = self.looked.get_mut(&key) else {
            return;
        };
        looked.reading = false;
        looked.checked = Some(now);
        looked.print = Some(print);
        let mut marked = None;
        let state = match result {
            Ok(mut read) => {
                let marks = self.store.reviewed(&read.root).unwrap_or_default();
                mirror::mark(&mut read.data, &read.hashes, &marks);
                // The whole branch, all of it read: a mark on a file not in it is gone.
                let whole = key.1 == DiffMode::Branch
                    && read.data.more == 0
                    && !read.data.base.starts_with("HEAD");
                if whole {
                    let files: Vec<String> =
                        read.data.files.iter().map(|f| f.path.clone()).collect();
                    if let Err(e) = self.store.keep_reviewed(&read.root, &files) {
                        tracing::warn!(error = %e, "could not forget reviewed marks");
                    }
                }
                marked = Some(read.root.clone());
                let looked = self.looked.get_mut(&key).expect("looked at above");
                looked.root = Some(read.root);
                looked.last = Some(Box::new(read.data));
                ReadState::Ready
            }
            Err(why) => ReadState::Failed(why),
        };
        let looked = self.looked.get_mut(&key).expect("looked at above");
        let (lookers, diff) = (looked.lookers.clone(), looked.last.clone());
        let again = std::mem::take(&mut looked.again);
        for client in lookers {
            self.send(
                client,
                ServerEvent::LocalDiff {
                    path: key.0.clone(),
                    mode: key.1,
                    state: state.clone(),
                    diff: diff.clone(),
                },
            );
        }
        if again {
            self.read_local(key);
        }
        if let Some(root) = marked {
            self.note_reviewed(&root);
        }
    }

    /// Marks `file` of the diff at `worktree` reviewed as it is now, or not; every
    /// client looking at that folder sees it at once, and its band counts it.
    pub(super) fn set_reviewed(&mut self, worktree: &Path, file: &str, reviewed: bool) {
        // The folder looked at may be inside the repo: its marks are the root's.
        let root = self
            .looked
            .iter()
            .find(|((path, _), _)| path == worktree)
            .and_then(|(_, l)| l.root.clone())
            .unwrap_or_else(|| place::resolved(worktree));
        let hash = reviewed.then(|| mirror::content_hash(&root, file));
        if let Err(e) = self.store.set_reviewed(&root, file, hash.as_deref()) {
            tracing::warn!(error = %e, "could not keep a reviewed mark");
            return;
        }
        let now = if reviewed {
            Viewed::Viewed
        } else {
            Viewed::Unviewed
        };
        let mut sends = vec![];
        for ((path, mode), looked) in &mut self.looked {
            if looked.root.as_ref() != Some(&root) {
                continue;
            }
            let Some(last) = looked.last.as_mut() else {
                continue;
            };
            for f in last.files.iter_mut().filter(|f| f.path == file) {
                f.viewed = now;
            }
            for client in &looked.lookers {
                sends.push((*client, path.clone(), *mode, looked.last.clone()));
            }
        }
        for (client, path, mode, diff) in sends {
            self.send(
                client,
                ServerEvent::LocalDiff {
                    path,
                    mode,
                    state: ReadState::Ready,
                    diff,
                },
            );
        }
        self.note_reviewed(&root);
    }

    /// Counts the folder's files still as they were when marked; a kept worktree's band
    /// shows it.
    fn note_reviewed(&mut self, root: &Path) {
        let marks = self.store.reviewed(root).unwrap_or_default();
        let count = mirror::count_reviewed(root, &marks);
        if self.reviewed_counts.insert(root.to_path_buf(), count) == Some(count) {
            return;
        }
        let project = self
            .store
            .worktrees()
            .unwrap_or_default()
            .into_iter()
            .find(|w| place::resolved(&w.path) == root)
            .map(|w| w.project);
        if let Some(project) = project {
            self.send_worktrees(project);
        }
    }

    /// The worktree at `root` is gone (`X`): so is its count.
    pub(super) fn forget_reviewed(&mut self, root: &Path) {
        self.reviewed_counts.remove(root);
    }

    /// Every folder someone looks at, checked when its last look is two seconds old;
    /// one never read (no repo found) waits for `R`.
    pub fn look_tick(&mut self, now: Instant) {
        for (key, looked) in &mut self.looked {
            let due = looked
                .checked
                .is_none_or(|t| now.saturating_duration_since(t) >= LOOK_EVERY);
            let Some(root) = looked.root.clone() else {
                continue;
            };
            if looked.reading || looked.checking || !due {
                continue;
            }
            looked.checking = true;
            let (tx, key) = (self.mirror_tx.clone(), key.clone());
            tokio::task::spawn_blocking(move || {
                let git = crate::github::worktree::git_raw;
                let print = std::panic::catch_unwind(|| mirror::fingerprint(&root, &git))
                    .ok()
                    .flatten();
                let _ = tx.send(Mirrored::Checked { key, print });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{connect, registry_on, run_git};
    use super::*;
    use termist_core::github::Patch;

    fn look(reg: &mut Registry, path: Option<&Path>, mode: DiffMode) {
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::SetLocalDiff {
                path: path.map(Path::to_path_buf),
                mode,
            },
        });
    }

    fn diff(ev: ServerEvent) -> (ReadState, Option<Box<LocalDiffData>>) {
        match ev {
            ServerEvent::LocalDiff { state, diff, .. } => (state, diff),
            other => panic!("{other:?}"),
        }
    }

    /// A repo on branch `fix` with `a.txt` changed and not committed.
    #[cfg(unix)]
    fn site(tmp: &Path) -> PathBuf {
        let site = tmp.join("site");
        std::fs::create_dir(&site).unwrap();
        run_git(&site, &["init", "-q", "-b", "main"]);
        std::fs::write(site.join("a.txt"), "one\n").unwrap();
        run_git(&site, &["add", "."]);
        run_git(&site, &["commit", "-q", "-m", "one"]);
        run_git(&site, &["switch", "-q", "-c", "fix"]);
        std::fs::write(site.join("a.txt"), "one\ntwo\n").unwrap();
        site
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_looked_at_diff_is_read_and_read_again_when_its_folder_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        let mut reg = registry_on(Store::open_in_memory());
        let mut done = reg.mirror_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        look(&mut reg, Some(&site), DiffMode::Branch);
        assert_eq!(
            diff(rx.try_recv().unwrap()),
            (ReadState::Reading, None),
            "at once"
        );
        let t0 = Instant::now();
        reg.mirrored(done.recv().await.unwrap(), t0);
        let (state, first) = diff(rx.try_recv().unwrap());
        assert_eq!(state, ReadState::Ready);
        let first = first.unwrap();
        assert_eq!((first.head.as_str(), first.base.as_str()), ("fix", "main"));
        assert_eq!(first.files.len(), 1);
        // Nothing changed: a check, no read.
        reg.look_tick(t0 + LOOK_EVERY);
        reg.mirrored(done.recv().await.unwrap(), t0 + LOOK_EVERY);
        assert!(rx.try_recv().is_err(), "nothing to send");
        reg.look_tick(t0 + LOOK_EVERY);
        assert!(done.try_recv().is_err(), "checked a moment ago");
        // The changed file changes again: read again.
        std::fs::write(site.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let t1 = t0 + LOOK_EVERY * 2;
        reg.look_tick(t1);
        reg.mirrored(done.recv().await.unwrap(), t1);
        reg.mirrored(done.recv().await.unwrap(), t1);
        let (_, second) = diff(rx.try_recv().unwrap());
        assert_eq!(second.unwrap().files[0].additions, 2);
        // `R`: read at once, the last diff kept meanwhile.
        look(&mut reg, Some(&site), DiffMode::Branch);
        let (state, kept) = diff(rx.try_recv().unwrap());
        assert_eq!((state, kept.is_some()), (ReadState::Reading, true));
        reg.mirrored(done.recv().await.unwrap(), t1);
        assert_eq!(diff(rx.try_recv().unwrap()).0, ReadState::Ready);
        // Looking away, or going: nothing is watched.
        look(&mut reg, None, DiffMode::Branch);
        assert!(reg.looked.is_empty());
        look(&mut reg, Some(&site), DiffMode::Uncommitted);
        reg.handle(Msg::Disconnected(ClientId(1)));
        assert!(reg.looked.is_empty() && reg.looking.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_diff_that_cannot_be_read_says_why_and_keeps_the_last_one() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        let mut reg = registry_on(Store::open_in_memory());
        let mut done = reg.mirror_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        look(&mut reg, Some(tmp.path()), DiffMode::Branch);
        rx.try_recv().unwrap();
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        assert_eq!(
            diff(rx.try_recv().unwrap()),
            (ReadState::Failed("not a git repository".into()), None)
        );
        look(&mut reg, Some(&site), DiffMode::Uncommitted);
        rx.try_recv().unwrap();
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        let (_, last) = diff(rx.try_recv().unwrap());
        assert!(matches!(last.unwrap().files[0].patch, Patch::Text(_)));
        std::fs::remove_dir_all(&site).unwrap();
        let t = Instant::now() + LOOK_EVERY;
        reg.look_tick(t);
        reg.mirrored(done.recv().await.unwrap(), t);
        reg.mirrored(done.recv().await.unwrap(), t);
        let (state, last) = diff(rx.try_recv().unwrap());
        assert_eq!(state, ReadState::Failed("the folder is gone".into()));
        assert!(last.is_some(), "the last diff stays");
        // Gone stays gone: no read every two seconds.
        let later = t + LOOK_EVERY;
        reg.look_tick(later);
        reg.mirrored(done.recv().await.unwrap(), later);
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_clients_on_one_folder_share_its_reads_until_the_last_looks_away() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        let mut reg = registry_on(Store::open_in_memory());
        let mut done = reg.mirror_rx.take().unwrap();
        let mut one = connect(&mut reg);
        let (out, mut two) = tokio::sync::mpsc::unbounded_channel();
        reg.handle(Msg::Connected {
            client: ClientId(2),
            out,
        });
        look(&mut reg, Some(&site), DiffMode::Branch);
        reg.handle(Msg::Request {
            client: ClientId(2),
            req: ClientRequest::SetLocalDiff {
                path: Some(site.clone()),
                mode: DiffMode::Branch,
            },
        });
        assert_eq!(reg.looked.len(), 1);
        let now = Instant::now();
        // The second ask came while the first read was on its way: one more read.
        reg.mirrored(done.recv().await.unwrap(), now);
        reg.mirrored(done.recv().await.unwrap(), now);
        assert!(done.try_recv().is_err());
        let ready = |rx: &mut UnboundedReceiver<ServerEvent>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter(|e| {
                    matches!(
                        e,
                        ServerEvent::LocalDiff {
                            state: ReadState::Ready,
                            ..
                        }
                    )
                })
                .count()
        };
        assert_eq!((ready(&mut one), ready(&mut two)), (2, 2));
        reg.handle(Msg::Disconnected(ClientId(1)));
        assert_eq!(reg.looked.len(), 1, "the other still looks");
        reg.handle(Msg::Disconnected(ClientId(2)));
        assert!(reg.looked.is_empty());
    }

    /// A project whose kept worktree is `site`.
    #[cfg(unix)]
    fn kept(site: &Path) -> (Store, ProjectId) {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "site".into(),
            path: site.parent().unwrap().to_path_buf(),
            open: true,
        };
        let store = Store::open_in_memory();
        store.upsert_project(&p).unwrap();
        store
            .upsert_worktree(&StoredWorktree {
                project: p.id,
                repo: None,
                path: place::resolved(site),
                branch: Some("fix".into()),
                base: Some("main".into()),
                pr: None,
                made_by_termist: true,
                shown: true,
            })
            .unwrap();
        (store, p.id)
    }

    fn mark(reg: &mut Registry, worktree: &Path, file: &str, reviewed: bool) {
        reg.handle(Msg::Request {
            client: ClientId(1),
            req: ClientRequest::SetReviewed {
                worktree: worktree.to_path_buf(),
                file: file.into(),
                reviewed,
            },
        });
    }

    fn events(rx: &mut UnboundedReceiver<ServerEvent>) -> Vec<ServerEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn viewed(evs: &[ServerEvent]) -> Option<Vec<(String, Viewed)>> {
        evs.iter().rev().find_map(|e| match e {
            ServerEvent::LocalDiff { diff: Some(d), .. } => {
                Some(d.files.iter().map(|f| (f.path.clone(), f.viewed)).collect())
            }
            _ => None,
        })
    }

    fn band(evs: &[ServerEvent]) -> Option<u32> {
        evs.iter().rev().find_map(|e| match e {
            ServerEvent::Worktrees { list, .. } => Some(list[0].reviewed),
            _ => None,
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_marked_reviewed_stays_so_until_it_changes_and_its_band_counts_it() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        std::fs::write(site.join("b.txt"), "new\n").unwrap();
        let (store, _) = kept(&site);
        let mut reg = registry_on(store);
        let mut done = reg.mirror_rx.take().unwrap();
        let mut rx = connect(&mut reg);
        look(&mut reg, Some(&site), DiffMode::Branch);
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        let a = |v| ("a.txt".to_string(), v);
        let b = |v| ("b.txt".to_string(), v);
        assert_eq!(
            viewed(&events(&mut rx)),
            Some(vec![a(Viewed::Unviewed), b(Viewed::Unviewed)])
        );
        mark(&mut reg, &site, "a.txt", true);
        let evs = events(&mut rx);
        assert_eq!(
            viewed(&evs),
            Some(vec![a(Viewed::Viewed), b(Viewed::Unviewed)]),
            "at once"
        );
        assert_eq!(band(&evs), Some(1));
        // Kept: a daemon that starts again knows it.
        assert_eq!(
            reg.store.reviewed(&place::resolved(&site)).unwrap().len(),
            1
        );
        // The agent changes it again: no longer reviewed, and the band says so.
        std::fs::write(site.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let t = Instant::now() + LOOK_EVERY;
        reg.look_tick(t);
        reg.mirrored(done.recv().await.unwrap(), t);
        reg.mirrored(done.recv().await.unwrap(), t);
        let evs = events(&mut rx);
        assert_eq!(
            viewed(&evs),
            Some(vec![a(Viewed::Dismissed), b(Viewed::Unviewed)])
        );
        assert_eq!(band(&evs), Some(0));
        mark(&mut reg, &site, "a.txt", true);
        mark(&mut reg, &site, "b.txt", true);
        assert_eq!(band(&events(&mut rx)), Some(2));
        mark(&mut reg, &site, "b.txt", false);
        let evs = events(&mut rx);
        assert_eq!(
            viewed(&evs),
            Some(vec![a(Viewed::Viewed), b(Viewed::Unviewed)])
        );
        assert_eq!(band(&evs), Some(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_gone_from_the_branch_s_diff_loses_its_mark_but_not_from_the_uncommitted() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        let (store, _) = kept(&site);
        let mut reg = registry_on(store);
        let mut done = reg.mirror_rx.take().unwrap();
        let _rx = connect(&mut reg);
        let root = place::resolved(&site);
        look(&mut reg, Some(&site), DiffMode::Branch);
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        mark(&mut reg, &site, "a.txt", true);
        // Committed: not in the uncommitted diff, but still reviewed.
        run_git(&site, &["commit", "-q", "-am", "two"]);
        look(&mut reg, Some(&site), DiffMode::Uncommitted);
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        assert_eq!(reg.store.reviewed(&root).unwrap().len(), 1);
        // Undone on the branch: out of its whole diff, so the mark goes.
        run_git(&site, &["revert", "--no-edit", "HEAD"]);
        look(&mut reg, Some(&site), DiffMode::Branch);
        reg.mirrored(done.recv().await.unwrap(), Instant::now());
        assert!(reg.store.reviewed(&root).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_scan_counts_a_worktree_s_reviewed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let site = site(tmp.path());
        let (store, project) = kept(&site);
        let root = place::resolved(&site);
        store
            .set_reviewed(&root, "a.txt", Some(&mirror::content_hash(&root, "a.txt")))
            .unwrap();
        let mut reg = registry_on(store);
        let mut rx = connect(&mut reg);
        let repo = termist_core::github::RepoId(7);
        reg.worktrees_scanned(WorktreeScan {
            project,
            repo,
            epoch: 0,
            found: Some(vec![]),
            reviewed: [(root.clone(), 1)].into(),
        });
        assert_eq!(band(&events(&mut rx)), Some(1));
        // `X`: its marks go with it.
        reg.forget_reviewed(&root);
        assert_eq!(reg.worktree_infos(project)[0].reviewed, 0);
    }
}
