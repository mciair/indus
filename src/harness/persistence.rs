//! SQLite-backed harness session storage.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};

use super::session::{Session, SessionMessage};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub directory: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub first_prompt: String,
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn database() -> Result<Self> {
        let root = data_root().ok_or_else(|| {
            anyhow!("Could not determine the user data directory for Indus sessions")
        })?;
        let directory = root.join("indus");
        fs::create_dir_all(&directory)
            .with_context(|| format!("Could not create {}", directory.display()))?;
        secure_directory(&directory)?;
        let store = Self {
            path: directory.join("indus.db"),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn load(&self, id: &str) -> Result<Option<Session>> {
        if !id.starts_with("ses-i_") {
            return Ok(None);
        }
        let connection = self.connect()?;
        let metadata = connection
            .query_row(
                "SELECT id, title, directory, provider_id, model_id, created_at, updated_at
                 FROM session WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .context("Could not read the Indus session")?;
        let Some((id, title, directory, provider_id, model_id, created_at, updated_at)) = metadata
        else {
            return Ok(None);
        };

        let mut statement = connection
            .prepare("SELECT data FROM message WHERE session_id = ?1 ORDER BY position ASC")
            .context("Could not prepare the Indus message query")?;
        let rows = statement
            .query_map(params![id], |row| row.get::<_, String>(0))
            .context("Could not read the Indus session messages")?;
        let mut messages = Vec::new();
        for row in rows {
            let data = row.context("Could not read an Indus message")?;
            messages.push(
                serde_json::from_str(&data).context("Stored Indus message is not valid JSON")?,
            );
        }
        Ok(Some(Session::restore(
            id,
            title,
            directory,
            provider_id,
            model_id,
            created_at,
            updated_at,
            messages,
        )))
    }

    pub fn list(&self, query: Option<&str>) -> Result<Vec<SessionSummary>> {
        let connection = self.connect()?;
        let pattern = format!("%{}%", query.unwrap_or_default().trim());
        let mut statement = connection
            .prepare(
                "SELECT s.id, s.title, s.directory, s.provider_id, s.model_id,
                        s.created_at, s.updated_at, COUNT(m.position)
                 FROM session s
                 LEFT JOIN message m ON m.session_id = s.id
                 WHERE s.title LIKE ?1 OR s.id LIKE ?1 OR s.directory LIKE ?1
                 GROUP BY s.id
                 ORDER BY s.updated_at DESC",
            )
            .context("Could not prepare the Indus session list")?;
        let rows = statement
            .query_map(params![pattern], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    directory: row.get(2)?,
                    provider_id: row.get(3)?,
                    model_id: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    message_count: row.get::<_, i64>(7)?.max(0) as usize,
                    first_prompt: String::new(),
                })
            })
            .context("Could not list Indus sessions")?;
        let mut sessions = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for session in &mut sessions {
            session.first_prompt = first_prompt(&connection, &session.id)?;
        }
        Ok(sessions)
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        if !session.is_allocated() {
            return Ok(());
        }
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .context("Could not start the Indus session transaction")?;
        transaction
            .execute(
                "INSERT INTO session
                    (id, title, directory, provider_id, model_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    directory = excluded.directory,
                    provider_id = excluded.provider_id,
                    model_id = excluded.model_id,
                    updated_at = excluded.updated_at",
                params![
                    session.id,
                    session.title.as_deref().unwrap_or_default(),
                    session.directory,
                    session.provider_id,
                    session.model_id,
                    session.created_at,
                    session.updated_at,
                ],
            )
            .context("Could not save the Indus session")?;
        transaction
            .execute(
                "DELETE FROM message WHERE session_id = ?1",
                params![session.id],
            )
            .context("Could not replace the Indus session messages")?;
        {
            let mut insert = transaction
                .prepare("INSERT INTO message (session_id, position, data) VALUES (?1, ?2, ?3)")
                .context("Could not prepare the Indus message writer")?;
            for (position, message) in session.messages.iter().enumerate() {
                let data = serde_json::to_string(message)
                    .context("Could not serialize an Indus session message")?;
                insert
                    .execute(params![session.id, position as i64, data])
                    .context("Could not save an Indus session message")?;
            }
        }
        transaction
            .commit()
            .context("Could not commit the Indus session")
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        if !id.starts_with("ses-i_") {
            return Ok(false);
        }
        let connection = self.connect()?;
        let deleted = connection
            .execute("DELETE FROM session WHERE id = ?1", params![id])
            .context("Could not delete the Indus session")?;
        Ok(deleted > 0)
    }

    fn initialize(&self) -> Result<()> {
        let connection = self.connect()?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS session (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    directory TEXT NOT NULL,
                    provider_id TEXT,
                    model_id TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS message (
                    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    data TEXT NOT NULL,
                    PRIMARY KEY (session_id, position)
                );
                CREATE INDEX IF NOT EXISTS session_updated_idx ON session(updated_at DESC);
                PRAGMA user_version = 1;",
            )
            .context("Could not initialize the Indus session database")?;
        secure_file(&self.path)
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("Could not open {}", self.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        Ok(connection)
    }

    #[cfg(test)]
    pub(super) fn at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let store = Self { path };
        store.initialize()?;
        Ok(store)
    }
}

fn first_prompt(connection: &Connection, session_id: &str) -> Result<String> {
    let mut statement = connection
        .prepare("SELECT data FROM message WHERE session_id = ?1 ORDER BY position ASC")?;
    let rows = statement.query_map(params![session_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        let message: SessionMessage = serde_json::from_str(&row?)?;
        if let SessionMessage::User(message) = message {
            return Ok(message.text);
        }
    }
    Ok(String::new())
}

fn data_root() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_store() -> (PathBuf, SessionStore) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("indus-session-test-{unique}"));
        let store = SessionStore::at(root.join("indus.db")).unwrap();
        (root, store)
    }

    #[test]
    fn unallocated_conversations_do_not_create_session_rows() {
        let (root, store) = temporary_store();
        let mut session = Session::unallocated("/workspace");
        session.push_user("not titled yet");
        store.save(&session).unwrap();

        assert!(store.list(None).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allocated_sessions_survive_a_store_reload() {
        let (root, store) = temporary_store();
        let mut session = Session::unallocated("/workspace");
        session.push_user("remember this");
        assert!(session.allocate(
            "ses-i_example",
            "Remember This Session",
            Some("groq".into()),
            Some("model".into()),
        ));
        store.save(&session).unwrap();

        assert_eq!(store.load("ses-i_example").unwrap(), Some(session.clone()));
        let summaries = store.list(Some("Remember")).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].first_prompt, "remember this");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_a_session_removes_its_messages() {
        let (root, store) = temporary_store();
        let mut session = Session::unallocated("/workspace");
        session.push_user("delete this");
        assert!(session.allocate("ses-i_delete", "Delete Me", None, None));
        store.save(&session).unwrap();

        assert!(store.delete("ses-i_delete").unwrap());
        assert_eq!(store.load("ses-i_delete").unwrap(), None);
        assert!(store.list(None).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
