//! Persistent storage. Opening the database is explicit; current snapshots
//! remain independent of the database and its configuration.

mod sqlite;

use std::{env, ffi::OsString, fmt, io, path::PathBuf};

pub use sqlite::Database;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Sqlite(rusqlite::Error),
    UnsupportedSchema(i64),
    InvalidSample(&'static str),
    InvalidTimeRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "database filesystem error: {error}"),
            Self::Sqlite(error) => write!(f, "SQLite error: {error}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported Lantern database schema version: {version}")
            }
            Self::InvalidSample(field) => write!(f, "non-finite metric value: {field}"),
            Self::InvalidTimeRange => write!(f, "history range ends before it starts"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// LANTERN_DB overrides XDG_STATE_HOME, then ~/.local/state/lantern/lantern.db.
/// Relative LANTERN_DB paths are relative to the process's working directory.
pub fn default_db_path() -> io::Result<PathBuf> {
    resolve_db_path(
        env::var_os("LANTERN_DB"),
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
    )
}

fn resolve_db_path(
    db: Option<OsString>,
    state: Option<OsString>,
    home: Option<OsString>,
) -> io::Result<PathBuf> {
    if let Some(db) = db {
        if db.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LANTERN_DB is empty",
            ));
        }
        return Ok(PathBuf::from(db));
    }
    if let Some(state) = state.map(PathBuf::from).filter(|path| path.is_absolute()) {
        return Ok(state.join("lantern/lantern.db"));
    }
    let home = home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "set LANTERN_DB, XDG_STATE_HOME, or an absolute HOME directory",
            )
        })?;
    Ok(home.join(".local/state/lantern/lantern.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_path_override_takes_precedence_and_allows_relative_paths() {
        for path in ["/tmp/custom.db", "local.db"] {
            assert_eq!(
                resolve_db_path(
                    Some(path.into()),
                    Some("/state".into()),
                    Some("/home/user".into())
                )
                .unwrap(),
                PathBuf::from(path)
            );
        }
    }

    #[test]
    fn storage_path_uses_xdg_or_home_fallback() {
        assert_eq!(
            resolve_db_path(None, Some("/state".into()), None).unwrap(),
            PathBuf::from("/state/lantern/lantern.db")
        );
        for state in [None, Some("".into()), Some("relative".into())] {
            assert_eq!(
                resolve_db_path(None, state, Some("/home/user".into())).unwrap(),
                PathBuf::from("/home/user/.local/state/lantern/lantern.db")
            );
        }
    }

    #[test]
    fn storage_path_errors_are_explicit() {
        assert!(resolve_db_path(Some("".into()), None, Some("/home/user".into())).is_err());
        assert!(resolve_db_path(None, None, None).is_err());
        assert!(resolve_db_path(None, None, Some("relative".into())).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn storage_path_preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"/tmp/lantern-\xff.db".to_vec());
        assert_eq!(
            resolve_db_path(Some(path.clone()), None, None).unwrap(),
            PathBuf::from(path)
        );
    }
}
