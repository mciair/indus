//! Durable harness session storage.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::session::Session;

#[derive(Clone, Debug)]
pub struct SessionStore {
    path: Option<PathBuf>,
}

impl SessionStore {
    pub fn default_session() -> Self {
        Self {
            path: config_root()
                .map(|root| root.join("indus").join("sessions").join("default.json")),
        }
    }

    pub fn load(&self) -> Session {
        self.path
            .as_ref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, session: &Session) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Session path has no parent"))?;
        fs::create_dir_all(parent)?;
        secure_directory(parent)?;
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(session).map_err(io::Error::other)?;
        write_private_file(&temporary, &bytes)?;
        fs::rename(&temporary, path)?;
        secure_file(path)
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
}

fn config_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn sessions_survive_a_store_reload() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("indus-session-test-{id}"));
        let store = SessionStore::at(root.join("default.json"));
        let mut session = Session::new("persistent");
        session.push_user("remember this");
        store.save(&session).unwrap();
        assert_eq!(store.load(), session);
        let _ = fs::remove_dir_all(root);
    }
}
