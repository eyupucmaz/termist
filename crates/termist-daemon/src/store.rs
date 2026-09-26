//! Persistence (PRD §11.6): projects and sessions in SQLite. Status is not stored:
//! a stored session has no process in a new daemon, so it loads as Disconnected.
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use termist_core::{
    AgentStatus, Harness, ProjectId, ProjectInfo, SessionId, SessionInfo, SessionKind, now_ms,
};

pub const SCHEMA_VERSION: i64 = 1;

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

pub struct Store {
    conn: Connection,
}

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

    fn migrate(conn: Connection) -> anyhow::Result<Store> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // Safety probe: a garbage file will fail here
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => conn.execute_batch(SCHEMA_V1)?,
            SCHEMA_VERSION => {}
            newer => anyhow::bail!(
                "schema version {newer} is newer than this termist ({SCHEMA_VERSION})"
            ),
        }
        // A table of the right version but the wrong shape is as unusable as garbage.
        conn.prepare(
            "SELECT id, project_id, kind, name, agent_session_id, title, last_activity_ms,
                    created_ms, resumable FROM sessions LIMIT 0",
        )?;
        Ok(Store { conn })
    }

    pub fn upsert_project(&self, p: &ProjectInfo) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, name, path, created_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, path = excluded.path",
            params![
                p.id.to_string(),
                p.name,
                p.path.to_string_lossy(),
                now_ms() as i64
            ],
        )?;
        Ok(())
    }

    pub fn upsert_session(&self, s: &SessionInfo, resumable: bool) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, project_id, kind, name, agent_session_id, title, last_activity_ms, created_ms, resumable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, agent_session_id = excluded.agent_session_id,
               title = excluded.title, last_activity_ms = excluded.last_activity_ms,
               resumable = excluded.resumable",
            params![
                s.id.to_string(),
                s.project.to_string(),
                encode_kind(&s.kind),
                s.name,
                s.agent_session_id,
                s.title,
                s.last_activity_ms as i64,
                now_ms() as i64,
                resumable
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
            .prepare("SELECT id, name, path FROM projects ORDER BY created_ms, rowid")?;
        let projects = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|row| row.ok())
            .filter_map(|(id, name, path)| {
                Some(ProjectInfo {
                    id: id.parse::<ProjectId>().ok()?,
                    name,
                    path: PathBuf::from(path),
                })
            })
            .collect();
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, kind, name, agent_session_id, title, last_activity_ms, resumable
             FROM sessions ORDER BY created_ms, rowid",
        )?;
        let sessions = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, bool>(7)?,
                ))
            })?
            .filter_map(|row| row.ok())
            .filter_map(
                |(id, project, kind, name, agent_session_id, title, last, resumable)| {
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
                        },
                        resumable,
                    })
                },
            )
            .collect();
        Ok((projects, sessions))
    }
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

    // Review Focus 1
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

    // Review Focus 1
    #[test]
    fn a_database_from_a_newer_termist_is_moved_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("termist.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.load().unwrap().1.is_empty());
        assert!(std::fs::read_dir(tmp.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("termist.db.broken-")
        }));
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

    // Review Focus 1: leftover journal is handled safely
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
}
