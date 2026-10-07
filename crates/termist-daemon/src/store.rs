//! Persistence: projects, sessions, the prompt history and the quick prompt's last
//! choice, in SQLite. Status is not stored: a stored session has no process in a new
//! daemon, so it loads as Disconnected.
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use termist_core::github::RepoId;
use termist_core::{
    AgentStatus, Harness, LaunchOptions, ProjectId, ProjectInfo, SessionId, SessionInfo,
    SessionKind, now_ms,
};

pub const SCHEMA_VERSION: i64 = 4;

/// How many prompts the history keeps.
pub const PROMPT_HISTORY_MAX: usize = 200;

/// How many recently used models are kept per harness.
pub const RECENT_MODELS_MAX: usize = 8;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    agent_session_id TEXT,
    title TEXT,
    last_activity_ms INTEGER NOT NULL,
    created_ms INTEGER NOT NULL,
    resumable INTEGER NOT NULL DEFAULT 0
);
PRAGMA user_version = 1;
";

/// Schema v1 as released, brought to v2. Runs in one transaction: a failure leaves the
/// v1 file as it was.
const MIGRATE_V2: &str = "
ALTER TABLE sessions ADD COLUMN model TEXT;
ALTER TABLE sessions ADD COLUMN effort TEXT;
ALTER TABLE sessions ADD COLUMN user_named INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN open INTEGER NOT NULL DEFAULT 1;
CREATE TABLE prompt_history (
    id INTEGER PRIMARY KEY,
    prompt TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE TABLE ui_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
PRAGMA user_version = 2;
";

/// v2 brought to v3: the GitHub repos found in projects and the pull requests seen.
const MIGRATE_V3: &str = "
CREATE TABLE gh_repo (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    visible INTEGER NOT NULL DEFAULT 1,
    account_override TEXT,
    UNIQUE (project_id, path)
);
CREATE TABLE pr_seen (
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    number INTEGER NOT NULL,
    seen_updated_at TEXT NOT NULL,
    PRIMARY KEY (owner, name, number)
);
PRAGMA user_version = 3;
";

/// v3 brought to v4: the folder each session runs in. Empty for sessions stored before:
/// their project's folder.
const MIGRATE_V4: &str = "
ALTER TABLE sessions ADD COLUMN cwd TEXT;
PRAGMA user_version = 4;
";

/// A GitHub repo found in a project, with the user's choices for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRepo {
    pub id: RepoId,
    pub project: ProjectId,
    pub path: PathBuf,
    pub owner: String,
    pub name: String,
    pub visible: bool,
    /// Set by the user; `None` picks the account with the most access.
    pub account: Option<String>,
}

pub struct Store {
    conn: Connection,
    /// Why this store is in memory although a file was asked for, as clients are told.
    not_saved: Option<String>,
}

/// The file's schema is one a newer termist wrote: this one must not touch it.
#[derive(Debug)]
struct NewerSchema(i64);

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "schema version {} is newer than this termist ({SCHEMA_VERSION})",
            self.0
        )
    }
}

impl std::error::Error for NewerSchema {}

/// A session as stored. `resumable` is true once the agent's own conversation exists
/// (a prompt was submitted, or the agent reported its id), so `--resume` can find it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredSession {
    pub info: SessionInfo,
    pub resumable: bool,
}

pub fn encode_kind(kind: &SessionKind) -> &'static str {
    match kind {
        SessionKind::Shell => "shell",
        SessionKind::Agent { harness } => harness.id(),
    }
}

pub fn decode_kind(s: &str) -> Option<SessionKind> {
    match s {
        "shell" => Some(SessionKind::Shell),
        other => Harness::from_id(other).map(|harness| SessionKind::Agent { harness }),
    }
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        match Self::try_open(path) {
            Ok(store) => Ok(store),
            Err(e) => {
                // Moving it aside would hide the newer termist's sessions from it.
                if let Some(NewerSchema(version)) = e.downcast_ref() {
                    tracing::warn!(error = %e, path = %path.display(), "database left alone; sessions are not saved");
                    let mut store = Self::open_in_memory();
                    store.not_saved = Some(format!(
                        "sessions are not being saved: the database was written by a newer \
                         termist (schema {version}) — upgrade termist"
                    ));
                    return Ok(store);
                }
                let aside = PathBuf::from(format!("{}.broken-{}", path.display(), now_ms()));
                tracing::warn!(error = %e, aside = %aside.display(), "database unusable; starting fresh");

                // Move main file aside
                if let Err(rename_err) = std::fs::rename(path, &aside) {
                    tracing::warn!(
                        error = %rename_err,
                        "failed to move database aside; using in-memory store"
                    );
                    return Ok(Self::open_in_memory());
                }

                // Move side files aside too (if they still exist)
                for suffix in &["-journal", "-wal", "-shm"] {
                    let side_path = PathBuf::from(format!("{}{}", path.display(), suffix));
                    if side_path.exists() {
                        let aside_side = PathBuf::from(format!("{}{}", aside.display(), suffix));
                        if let Err(e) = std::fs::rename(&side_path, &aside_side) {
                            tracing::warn!(
                                error = %e,
                                path = %side_path.display(),
                                "failed to move database side file aside"
                            );
                        }
                    }
                }

                // Retry opening with fresh database
                match Self::try_open(path) {
                    Ok(store) => Ok(store),
                    Err(retry_err) => {
                        tracing::warn!(
                            error = %retry_err,
                            "failed to create fresh database; using in-memory store"
                        );
                        Ok(Self::open_in_memory())
                    }
                }
            }
        }
    }

    pub fn open_in_memory() -> Store {
        Self::migrate(Connection::open_in_memory().expect("in-memory sqlite"))
            .expect("fresh schema")
    }

    fn try_open(path: &Path) -> anyhow::Result<Store> {
        Self::migrate(Connection::open(path)?)
    }

    /// Nothing is written before the version is known to be one this termist can use.
    fn migrate(mut conn: Connection) -> anyhow::Result<Store> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // Safety probe: a garbage file will fail here
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(NewerSchema(version).into());
        }
        if conn.path().is_some_and(|p| !p.is_empty()) {
            // Every status change is a write; WAL with NORMAL sync keeps them cheap.
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        match version {
            0 => {
                conn.execute_batch(SCHEMA_V1)?;
                Self::upgrade(&mut conn, MIGRATE_V2)?;
                Self::upgrade(&mut conn, MIGRATE_V3)?;
                Self::upgrade(&mut conn, MIGRATE_V4)?;
            }
            1 => {
                Self::upgrade(&mut conn, MIGRATE_V2)?;
                Self::upgrade(&mut conn, MIGRATE_V3)?;
                Self::upgrade(&mut conn, MIGRATE_V4)?;
            }
            2 => {
                Self::upgrade(&mut conn, MIGRATE_V3)?;
                Self::upgrade(&mut conn, MIGRATE_V4)?;
            }
            3 => Self::upgrade(&mut conn, MIGRATE_V4)?,
            SCHEMA_VERSION => {}
            other => anyhow::bail!("unknown schema version {other}"),
        }
        // A table of the right version but the wrong shape is as unusable as garbage.
        conn.prepare(
            "SELECT id, project_id, kind, name, agent_session_id, title, last_activity_ms,
                    created_ms, resumable, model, effort, user_named, archived, cwd
             FROM sessions LIMIT 0",
        )?;
        conn.prepare("SELECT id, name, path, created_ms, open FROM projects LIMIT 0")?;
        conn.prepare("SELECT id, prompt, created_ms FROM prompt_history LIMIT 0")?;
        conn.prepare("SELECT key, value FROM ui_state LIMIT 0")?;
        conn.prepare(
            "SELECT id, project_id, path, owner, name, visible, account_override FROM gh_repo LIMIT 0",
        )?;
        conn.prepare("SELECT owner, name, number, seen_updated_at FROM pr_seen LIMIT 0")?;
        Ok(Store {
            conn,
            not_saved: None,
        })
    }

    /// Why nothing is being saved, when the database could not be used as it is.
    pub fn not_saved(&self) -> Option<&str> {
        self.not_saved.as_deref()
    }

    /// Runs one migration in a transaction: a failure leaves the file as it was.
    fn upgrade(conn: &mut Connection, sql: &str) -> anyhow::Result<()> {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_project(&self, p: &ProjectInfo) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, name, path, created_ms, open) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, path = excluded.path,
               open = excluded.open",
            params![
                p.id.to_string(),
                p.name,
                p.path.to_string_lossy(),
                now_ms() as i64,
                p.open
            ],
        )?;
        Ok(())
    }

    pub fn upsert_session(&self, s: &SessionInfo, resumable: bool) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, project_id, kind, name, agent_session_id, title, last_activity_ms,
                                   created_ms, resumable, model, effort, user_named, archived, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, agent_session_id = excluded.agent_session_id,
               title = excluded.title, last_activity_ms = excluded.last_activity_ms,
               resumable = excluded.resumable, model = excluded.model, effort = excluded.effort,
               user_named = excluded.user_named, archived = excluded.archived, cwd = excluded.cwd",
            params![
                s.id.to_string(),
                s.project.to_string(),
                encode_kind(&s.kind),
                s.name,
                s.agent_session_id,
                s.title,
                s.last_activity_ms as i64,
                now_ms() as i64,
                resumable,
                s.model,
                s.effort,
                s.user_named,
                s.archived,
                s.cwd.to_string_lossy()
            ],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: SessionId) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    pub fn load(&self) -> anyhow::Result<(Vec<ProjectInfo>, Vec<StoredSession>)> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path, open FROM projects ORDER BY created_ms, rowid")?;
        let projects = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })?
            .filter_map(|row| row.ok())
            .filter_map(|(id, name, path, open)| {
                Some(ProjectInfo {
                    id: id.parse::<ProjectId>().ok()?,
                    name,
                    path: PathBuf::from(path),
                    open,
                })
            })
            .collect();
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, kind, name, agent_session_id, title, last_activity_ms, resumable,
                    model, effort, user_named, archived, cwd
             FROM sessions ORDER BY created_ms, rowid",
        )?;
        let sessions = stmt
            .query_map([], |r| {
                Ok((
                    (
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, bool>(7)?,
                    ),
                    (
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, bool>(10)?,
                        r.get::<_, bool>(11)?,
                        r.get::<_, Option<String>>(12)?,
                    ),
                ))
            })?
            .filter_map(|row| row.ok())
            .filter_map(
                |(
                    (id, project, kind, name, agent_session_id, title, last, resumable),
                    (model, effort, user_named, archived, cwd),
                )| {
                    Some(StoredSession {
                        info: SessionInfo {
                            id: id.parse::<SessionId>().ok()?,
                            project: project.parse::<ProjectId>().ok()?,
                            kind: decode_kind(&kind)?,
                            name,
                            status: AgentStatus::Disconnected,
                            agent_session_id,
                            title,
                            last_activity_ms: last.max(0) as u64,
                            model,
                            effort,
                            user_named,
                            archived,
                            cwd: cwd.map(PathBuf::from).unwrap_or_default(),
                            place: None,
                        },
                        resumable,
                    })
                },
            )
            .collect();
        Ok((projects, sessions))
    }

    /// Adds a prompt to the history, unless it repeats the newest one, and keeps only
    /// the newest `PROMPT_HISTORY_MAX`.
    pub fn add_prompt(&self, prompt: &str) -> anyhow::Result<()> {
        let newest: Option<String> = self
            .conn
            .query_row(
                "SELECT prompt FROM prompt_history ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if newest.as_deref() == Some(prompt) {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO prompt_history (prompt, created_ms) VALUES (?1, ?2)",
            params![prompt, now_ms() as i64],
        )?;
        self.conn.execute(
            "DELETE FROM prompt_history WHERE id NOT IN
               (SELECT id FROM prompt_history ORDER BY id DESC LIMIT ?1)",
            params![PROMPT_HISTORY_MAX as i64],
        )?;
        Ok(())
    }

    /// Up to `limit` prompts, newest first.
    pub fn prompt_history(&self, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT prompt FROM prompt_history ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn ui_value(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM ui_state WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn set_ui_value(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO ui_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// The quick prompt's last choice; `None` if never set or unreadable.
    pub fn last_launch(&self) -> Option<LaunchOptions> {
        let json = self.ui_value("last_launch").ok()??;
        serde_json::from_str(&json).ok()
    }

    pub fn set_last_launch(&self, launch: &LaunchOptions) -> anyhow::Result<()> {
        self.set_ui_value("last_launch", &serde_json::to_string(launch)?)
    }

    /// Models recently started with `harness`, most recent first.
    pub fn recent_models(&self, harness: Harness) -> Vec<String> {
        self.ui_value(&recent_models_key(harness))
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Moves `model` to the front of the harness's recent models (at most `RECENT_MODELS_MAX`).
    pub fn add_recent_model(&self, harness: Harness, model: &str) -> anyhow::Result<()> {
        let mut models = self.recent_models(harness);
        models.retain(|m| m != model);
        models.insert(0, model.to_string());
        models.truncate(RECENT_MODELS_MAX);
        self.set_ui_value(
            &recent_models_key(harness),
            &serde_json::to_string(&models)?,
        )
    }

    /// Records a repo found in `project`; one found before keeps its id and choices.
    pub fn upsert_repo(
        &self,
        project: ProjectId,
        path: &Path,
        owner: &str,
        name: &str,
    ) -> anyhow::Result<StoredRepo> {
        self.conn.execute(
            "INSERT INTO gh_repo (project_id, path, owner, name) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, path) DO UPDATE SET owner = excluded.owner, name = excluded.name",
            params![project.to_string(), path.to_string_lossy(), owner, name],
        )?;
        let repo = self.conn.query_row(
            "SELECT id, project_id, path, owner, name, visible, account_override FROM gh_repo
             WHERE project_id = ?1 AND path = ?2",
            params![project.to_string(), path.to_string_lossy()],
            stored_repo,
        )?;
        repo.ok_or_else(|| anyhow::anyhow!("a stored repo has a bad project id"))
    }

    pub fn repos(&self) -> anyhow::Result<Vec<StoredRepo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, owner, name, visible, account_override FROM gh_repo ORDER BY id",
        )?;
        let rows = stmt.query_map([], stored_repo)?;
        let mut repos = Vec::new();
        for row in rows {
            if let Some(repo) = row? {
                repos.push(repo);
            }
        }
        Ok(repos)
    }

    pub fn set_repo_visible(&self, id: RepoId, visible: bool) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE gh_repo SET visible = ?2 WHERE id = ?1",
            params![id.0, visible],
        )?;
        Ok(())
    }

    pub fn set_repo_account(&self, id: RepoId, account: Option<&str>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE gh_repo SET account_override = ?2 WHERE id = ?1",
            params![id.0, account],
        )?;
        Ok(())
    }

    /// When each pull request was last opened, as `updatedAt` stood then.
    pub fn seen(&self) -> anyhow::Result<HashMap<(String, String, u32), String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT owner, name, number, seen_updated_at FROM pr_seen")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ),
                r.get::<_, String>(3)?,
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn mark_seen(
        &self,
        owner: &str,
        name: &str,
        number: u32,
        updated_at: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO pr_seen (owner, name, number, seen_updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, name, number) DO UPDATE SET seen_updated_at = excluded.seen_updated_at",
            params![owner, name, number, updated_at],
        )?;
        Ok(())
    }
}

/// A `gh_repo` row; `None` when its project id is not a uuid.
fn stored_repo(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<StoredRepo>> {
    let project: String = r.get(1)?;
    let Ok(project) = project.parse::<ProjectId>() else {
        return Ok(None);
    };
    Ok(Some(StoredRepo {
        id: RepoId(r.get(0)?),
        project,
        path: PathBuf::from(r.get::<_, String>(2)?),
        owner: r.get(3)?,
        name: r.get(4)?,
        visible: r.get(5)?,
        account: r.get(6)?,
    }))
}

fn recent_models_key(harness: Harness) -> String {
    format!("recent_models.{}", harness.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::Harness;

    fn project(path: &str) -> ProjectInfo {
        ProjectInfo {
            id: ProjectId::new(),
            name: "api".into(),
            path: PathBuf::from(path),
            open: true,
        }
    }

    fn session(project: ProjectId, kind: SessionKind, name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project,
            kind,
            name: name.into(),
            status: AgentStatus::Running,
            agent_session_id: Some("agent-1".into()),
            title: Some("Fix Login".into()),
            last_activity_ms: 42,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: "/p".into(),
            place: None,
        }
    }

    #[test]
    fn projects_and_sessions_survive_a_reopen_and_come_back_disconnected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        let p = project("/code/api");
        let a = session(
            p.id,
            SessionKind::Agent {
                harness: Harness::Codex,
            },
            "codex-1",
        );
        let b = session(p.id, SessionKind::Shell, "shell-2");
        {
            let store = Store::open(&path).unwrap();
            store.upsert_project(&p).unwrap();
            store.upsert_session(&a, true).unwrap();
            store.upsert_session(&b, false).unwrap();
        }
        let (projects, stored) = Store::open(&path).unwrap().load().unwrap();
        assert_eq!(
            stored.iter().map(|s| s.resumable).collect::<Vec<_>>(),
            vec![true, false],
            "the resumable flag survives a reopen"
        );
        let sessions: Vec<SessionInfo> = stored.into_iter().map(|s| s.info).collect();
        assert_eq!(projects, vec![p]);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, a.id, "ordered by creation");
        assert_eq!(
            sessions[0].kind,
            SessionKind::Agent {
                harness: Harness::Codex
            }
        );
        assert_eq!(sessions[0].agent_session_id.as_deref(), Some("agent-1"));
        assert_eq!(sessions[0].title.as_deref(), Some("Fix Login"));
        assert!(
            sessions
                .iter()
                .all(|s| s.status == AgentStatus::Disconnected)
        );
    }

    #[test]
    fn a_database_file_uses_the_write_ahead_log() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("termist.db")).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn upsert_updates_in_place_and_delete_removes() {
        let store = Store::open_in_memory();
        let p = project("/code/api");
        store.upsert_project(&p).unwrap();
        let mut s = session(p.id, SessionKind::Shell, "shell-1");
        store.upsert_session(&s, false).unwrap();
        s.name = "renamed".into();
        s.agent_session_id = None;
        store.upsert_session(&s, true).unwrap();
        let (_, sessions) = store.load().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].info.name, "renamed");
        assert_eq!(sessions[0].info.agent_session_id, None);
        assert!(sessions[0].resumable);
        store.delete_session(s.id).unwrap();
        assert!(store.load().unwrap().1.is_empty());
    }

    #[test]
    fn kinds_round_trip_through_their_text_form() {
        for kind in [
            SessionKind::Shell,
            SessionKind::Agent {
                harness: Harness::Claude,
            },
            SessionKind::Agent {
                harness: Harness::OpenCode,
            },
        ] {
            assert_eq!(decode_kind(encode_kind(&kind)), Some(kind));
        }
        assert_eq!(decode_kind("cursor"), None);
    }

    #[test]
    fn a_corrupt_database_is_moved_aside_and_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        std::fs::write(
            &path,
            b"this is not sqlite, just garbage bytes that fill a page",
        )
        .unwrap();
        let store = Store::open(&path).unwrap();
        assert!(store.load().unwrap().0.is_empty());
        assert_eq!(store.not_saved(), None, "the fresh file is saved to");
        let aside: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("termist.db.broken-")
            })
            .collect();
        assert_eq!(aside.len(), 1, "the bad file is kept for inspection");
    }

    // The newer termist's sessions stay where it will look for them; this one runs
    // without saving, and says so.
    #[test]
    fn a_database_from_a_newer_termist_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL; CREATE TABLE future (x); PRAGMA user_version = 99;",
            )
            .unwrap();
        }
        let listing = || {
            let mut names: Vec<_> = std::fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let (before, bytes) = (listing(), std::fs::read(&path).unwrap());
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.not_saved(),
            Some(
                "sessions are not being saved: the database was written by a newer \
                 termist (schema 99) — upgrade termist"
            )
        );
        store.upsert_project(&project("/p")).unwrap();
        assert_eq!(store.load().unwrap().0.len(), 1, "works in memory");
        drop(store);
        assert_eq!(listing(), before, "nothing moved, nothing added");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "not written");
    }

    #[test]
    fn a_database_missing_a_column_is_moved_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                &SCHEMA_V1.replace(",\n    resumable INTEGER NOT NULL DEFAULT 0", ""),
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let p = project("/code/api");
        store.upsert_project(&p).unwrap();
        store
            .upsert_session(&session(p.id, SessionKind::Shell, "shell-1"), false)
            .expect("the fresh database has every column");
        assert!(std::fs::read_dir(tmp.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("termist.db.broken-")
        }));
    }

    #[test]
    fn sessions_of_an_unknown_kind_are_skipped_not_fatal() {
        let store = Store::open_in_memory();
        let p = project("/code/api");
        store.upsert_project(&p).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, project_id, kind, name, last_activity_ms, created_ms) VALUES (?1, ?2, 'cursor', 'x', 0, 0)",
                rusqlite::params![SessionId::new().to_string(), p.id.to_string()],
            )
            .unwrap();
        assert!(store.load().unwrap().1.is_empty());
    }

    // A leftover journal next to a corrupt file must not get in the way of the new one.
    #[test]
    fn corrupt_database_with_leftover_journal_starts_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        let journal_path = tmp.path().join("termist.db-journal");

        // Create a corrupt database
        std::fs::write(&path, b"garbage").unwrap();
        // Create a leftover journal file that would interfere
        std::fs::write(&journal_path, b"old journal data").unwrap();

        // Open the corrupt database — should recover safely
        let store = Store::open(&path).unwrap();
        assert!(
            store.load().unwrap().0.is_empty(),
            "fresh store loads empty"
        );

        // Verify no stale journal remains at original location that could interfere
        assert!(
            !journal_path.exists(),
            "no journal at original location (SQLite auto-cleaned or was moved aside)"
        );

        // Verify corrupt file was moved aside
        let files: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            files
                .iter()
                .any(|name| name.starts_with("termist.db.broken-")),
            "corrupt database should be moved aside. Files: {:?}",
            files
        );
    }

    fn moved_aside(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("termist.db.broken-")
            })
            .map(|e| e.path())
            .filter(|p| {
                !p.to_string_lossy().ends_with("-wal") && !p.to_string_lossy().ends_with("-shm")
            })
            .collect()
    }

    // A released v1 database, with data, opened by this termist.
    #[test]
    fn a_v1_database_with_data_upgrades_to_v2_without_losing_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        let (project, session) = (ProjectId::new(), SessionId::new());
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, path, created_ms) VALUES (?1, 'api', '/code/api', 1)",
                params![project.to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, project_id, kind, name, agent_session_id, title,
                                       last_activity_ms, created_ms, resumable)
                 VALUES (?1, ?2, 'codex', 'codex-1', 'agent-1', 'Fix Login', 42, 2, 1)",
                params![session.to_string(), project.to_string()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(
            moved_aside(tmp.path()).is_empty(),
            "nothing was moved aside"
        );
        let (projects, sessions) = store.load().unwrap();
        assert_eq!(
            projects,
            vec![ProjectInfo {
                id: project,
                name: "api".into(),
                path: PathBuf::from("/code/api"),
                open: true,
            }]
        );
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert!(s.resumable);
        assert_eq!(s.info.id, session);
        assert_eq!(s.info.project, project);
        assert_eq!(
            s.info.kind,
            SessionKind::Agent {
                harness: Harness::Codex
            }
        );
        assert_eq!(s.info.name, "codex-1");
        assert_eq!(s.info.agent_session_id.as_deref(), Some("agent-1"));
        assert_eq!(s.info.title.as_deref(), Some("Fix Login"));
        assert_eq!(s.info.last_activity_ms, 42);
        assert_eq!(
            (s.info.model.as_deref(), s.info.effort.as_deref()),
            (None, None)
        );
        assert!(!s.info.user_named && !s.info.archived);
        assert!(store.prompt_history(10).unwrap().is_empty());
        assert_eq!(store.last_launch(), None);
    }

    #[test]
    fn a_v1_database_that_cannot_be_upgraded_is_moved_aside_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        {
            let conn = Connection::open(&path).unwrap();
            // a `model` column already there makes the upgrade's ALTER TABLE fail
            conn.execute_batch(&SCHEMA_V1.replace(
                "resumable INTEGER NOT NULL DEFAULT 0",
                "resumable INTEGER NOT NULL DEFAULT 0,\n    model TEXT",
            ))
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.load().unwrap().1.is_empty(), "a fresh store");
        let aside = moved_aside(tmp.path());
        assert_eq!(aside.len(), 1);
        let version: i64 = Connection::open(&aside[0])
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1, "the failed upgrade was rolled back");
    }

    #[test]
    fn the_v2_fields_survive_a_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        let mut p = project("/code/api");
        p.open = false;
        let mut s = session(
            p.id,
            SessionKind::Agent {
                harness: Harness::Claude,
            },
            "claude-1",
        );
        s.model = Some("claude opus \"4\"".into());
        s.effort = Some("max".into());
        s.user_named = true;
        s.archived = true;
        {
            let store = Store::open(&path).unwrap();
            store.upsert_project(&p).unwrap();
            store.upsert_session(&s, false).unwrap();
        }
        let (projects, sessions) = Store::open(&path).unwrap().load().unwrap();
        assert!(!projects[0].open);
        let back = &sessions[0].info;
        assert_eq!(back.model, s.model);
        assert_eq!(back.effort.as_deref(), Some("max"));
        assert!(back.user_named && back.archived);
    }

    #[test]
    fn the_prompt_history_keeps_the_newest_prompts_newest_first() {
        let store = Store::open_in_memory();
        for i in 0..PROMPT_HISTORY_MAX + 5 {
            store.add_prompt(&format!("p{i}")).unwrap();
        }
        store.add_prompt("p204").unwrap(); // repeats the newest: not added again
        let all = store.prompt_history(1000).unwrap();
        assert_eq!(all.len(), PROMPT_HISTORY_MAX);
        assert_eq!(all[0], "p204");
        assert_eq!(all[PROMPT_HISTORY_MAX - 1], "p5");
        assert_eq!(store.prompt_history(2).unwrap(), vec!["p204", "p203"]);
    }

    #[test]
    fn the_last_launch_and_recent_models_survive_a_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        let launch = LaunchOptions {
            harness: Harness::Codex,
            model: Some("gpt-5".into()),
            effort: Some("high".into()),
        };
        {
            let store = Store::open(&path).unwrap();
            store.set_last_launch(&launch).unwrap();
            for m in ["a", "b", "a"] {
                store.add_recent_model(Harness::Codex, m).unwrap();
            }
            store.add_recent_model(Harness::Claude, "opus").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.last_launch(), Some(launch));
        assert_eq!(store.recent_models(Harness::Codex), vec!["a", "b"]);
        assert_eq!(store.recent_models(Harness::Claude), vec!["opus"]);
        assert!(store.recent_models(Harness::OpenCode).is_empty());
        for i in 0..RECENT_MODELS_MAX + 3 {
            store
                .add_recent_model(Harness::Codex, &format!("m{i}"))
                .unwrap();
        }
        let recent = store.recent_models(Harness::Codex);
        assert_eq!(recent.len(), RECENT_MODELS_MAX);
        assert_eq!(recent[0], format!("m{}", RECENT_MODELS_MAX + 2));
    }

    #[test]
    fn repos_keep_their_choices_when_found_again() {
        let store = Store::open_in_memory();
        let p = project("/work");
        store.upsert_project(&p).unwrap();
        let site = store
            .upsert_repo(p.id, Path::new("/work/site"), "acme", "site")
            .unwrap();
        assert!(site.visible);
        assert_eq!(site.account, None);
        store.set_repo_visible(site.id, false).unwrap();
        store.set_repo_account(site.id, Some("alice-work")).unwrap();
        let again = store
            .upsert_repo(p.id, Path::new("/work/site"), "acme", "site-renamed")
            .unwrap();
        assert_eq!(again.id, site.id);
        assert!(!again.visible);
        assert_eq!(again.account.as_deref(), Some("alice-work"));
        assert_eq!(again.name, "site-renamed");
        store.set_repo_account(site.id, None).unwrap();
        assert_eq!(
            store.repos().unwrap(),
            [StoredRepo {
                account: None,
                ..again
            }]
        );
    }

    #[test]
    fn seen_marks_are_kept_per_pull_request() {
        let store = Store::open_in_memory();
        store
            .mark_seen("acme", "site", 212, "2026-10-02T10:00:00Z")
            .unwrap();
        store
            .mark_seen("acme", "site", 212, "2026-10-02T11:00:00Z")
            .unwrap();
        store
            .mark_seen("acme", "admin", 88, "2026-10-01T09:00:00Z")
            .unwrap();
        let seen = store.seen().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[&("acme".to_string(), "site".to_string(), 212)],
            "2026-10-02T11:00:00Z"
        );
    }

    #[test]
    fn a_v2_database_upgrades_to_the_newest_and_keeps_its_projects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("termist.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(MIGRATE_V2).unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, path, created_ms, open) VALUES (?1, 'api', '/api', 1, 1)",
                params![ProjectId::new().to_string()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let (projects, _) = store.load().unwrap();
        assert_eq!(projects.len(), 1);
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        store
            .upsert_repo(projects[0].id, Path::new("/api"), "acme", "api")
            .unwrap();
    }

    #[test]
    fn a_v3_session_has_no_folder_and_a_new_one_keeps_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("termist.db");
        let p = project("/code/api");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(MIGRATE_V2).unwrap();
            conn.execute_batch(MIGRATE_V3).unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, path, created_ms, open) VALUES (?1, 'api', '/code/api', 1, 1)",
                params![p.id.to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, project_id, kind, name, last_activity_ms, created_ms)
                 VALUES (?1, ?2, 'shell', 'shell-1', 0, 0)",
                params![SessionId::new().to_string(), p.id.to_string()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let (_, sessions) = store.load().unwrap();
        assert_eq!(sessions[0].info.cwd, PathBuf::new(), "the project's folder");
        let mut s = session(p.id, SessionKind::Shell, "shell-2");
        s.cwd = "/code/api-worktrees/fix/login".into();
        store.upsert_session(&s, false).unwrap();
        s.cwd = "/code/api-worktrees/fix/login/src".into();
        store.upsert_session(&s, false).unwrap();
        let (_, sessions) = store.load().unwrap();
        assert_eq!(
            sessions[1].info.cwd,
            PathBuf::from("/code/api-worktrees/fix/login/src")
        );
    }
}
