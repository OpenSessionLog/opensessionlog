use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::connector::Connector;
use crate::error::{OslError, Result};
use crate::model::{NormalizedSession, SessionRef};
use crate::recency::RecencyFilter;

/// Find a SQLite database file at `target`.
/// - If `target` is a file, return it directly.
/// - If `target` is a directory, look for `default_name` first; if not found,
///   fall back to any `.db` or `.sqlite` file in the directory.
/// - Otherwise, return an error.
pub fn resolve_db_path(target: &Path, default_name: &str) -> Result<PathBuf> {
    if target.is_dir() {
        let candidate = target.join(default_name);
        if candidate.exists() {
            Ok(candidate)
        } else {
            // Fall back: search for any .db or .sqlite file in the directory
            let mut found: Option<PathBuf> = None;
            if let Ok(entries) = std::fs::read_dir(target) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(ext) = path.extension() {
                            if ext == "db" || ext == "sqlite" {
                                found = Some(path);
                                break;
                            }
                        }
                    }
                }
            }
            found.ok_or_else(|| {
                OslError::Connector(format!(
                    "no {} found in directory {}",
                    default_name,
                    target.display()
                ))
            })
        }
    } else if target.is_file() {
        Ok(target.to_path_buf())
    } else {
        Err(OslError::Connector(format!(
            "path does not exist: {}",
            target.display()
        )))
    }
}

/// Open a SQLite connection with `PRAGMA busy_timeout=5000`.
pub fn open_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    Ok(conn)
}

/// Schema-specific logic for a SQLite-backed connector.
/// Implementors provide the queries and row-mapping;
/// `SqliteConnector` handles DB-opening and error-wrapping boilerplate.
pub trait SqliteSessionParser: Send + Sync {
    /// Stable connector name (e.g. "opencode", "hermes").
    fn source_name(&self) -> &'static str;

    /// Default database filename to look for (e.g. "opencode.db", "state.db").
    fn db_filename(&self) -> &'static str;

    /// Discover session refs, optionally filtered by recency.
    /// `filter` is `None` when called from `discover()`,
    /// `Some(active_filter)` when called from `discover_filtered()`.
    /// `db_path` is provided so `SessionRef.path` can be set correctly.
    fn discover_sessions(
        &self,
        conn: &Connection,
        db_path: &Path,
        filter: Option<&RecencyFilter>,
    ) -> Result<Vec<SessionRef>>;

    /// Parse a single session from the database.
    fn parse_session(
        &self,
        conn: &Connection,
        session_ref: &SessionRef,
    ) -> Result<NormalizedSession>;
}

/// A `Connector` implementation backed by a SQLite database.
/// Handles shared boilerplate: resolve DB path, open connection with PRAGMA,
/// delegate schema-specific work to `P`.
pub struct SqliteConnector<P: SqliteSessionParser> {
    pub parser: P,
}

impl<P: SqliteSessionParser> Connector for SqliteConnector<P> {
    fn name(&self) -> &'static str {
        self.parser.source_name()
    }

    fn discover(&self, target: &Path) -> Result<Vec<SessionRef>> {
        let db_path = resolve_db_path(target, self.parser.db_filename())?;
        let conn = open_db(&db_path)?;
        self.parser.discover_sessions(&conn, &db_path, None)
    }

    fn discover_filtered(&self, target: &Path, filter: &RecencyFilter) -> Result<Vec<SessionRef>> {
        if !filter.is_active() {
            return self.discover(target);
        }
        let db_path = resolve_db_path(target, self.parser.db_filename())?;
        let conn = open_db(&db_path)?;
        self.parser.discover_sessions(&conn, &db_path, Some(filter))
    }

    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession> {
        let conn = open_db(&session_ref.path)?;
        self.parser.parse_session(&conn, session_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_db_path_returns_file_directly() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let result = resolve_db_path(path, "opencode.db").unwrap();
        assert_eq!(result, path);
    }

    #[test]
    fn resolve_db_path_finds_default_in_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("opencode.db");
        std::fs::File::create(&db).unwrap();
        let result = resolve_db_path(tmp.path(), "opencode.db").unwrap();
        assert_eq!(result, db);
    }

    #[test]
    fn resolve_db_path_falls_back_to_any_db_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("custom.db");
        std::fs::File::create(&db).unwrap();
        let result = resolve_db_path(tmp.path(), "state.db").unwrap();
        assert_eq!(result, db);
    }

    #[test]
    fn resolve_db_path_empty_directory_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = resolve_db_path(tmp.path(), "opencode.db");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_db_path_nonexistent_path_errors() {
        let result = resolve_db_path(Path::new("/no/such/path/xyz"), "opencode.db");
        assert!(result.is_err());
    }

    #[test]
    fn open_db_sets_busy_timeout_pragma() {
        // rusqlite::Connection::open creates the file if it doesn't exist.
        let path = std::env::temp_dir().join("osl_sqlite_test_open.db");
        let _ = std::fs::remove_file(&path);
        let conn = open_db(&path).unwrap();
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        let _ = std::fs::remove_file(&path);
    }
}
