use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{OslError, Result};

pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Open a SQLite connection with the runtime pragmas required by OpenSessionLog.
/// WAL mode, foreign keys, and normal synchronous mode are set on every connection.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(conn)
}

/// Initialize a new vault at `path`. If the file already exists and `force` is false,
/// returns `AlreadyExists`. Otherwise opens (creating if necessary), runs schema DDL,
/// seeds config and source rows, and returns the canonical path.
pub fn init(path: &Path, force: bool) -> Result<PathBuf> {
    if path.exists() && !force {
        return Err(OslError::AlreadyExists(format!(
            "vault already exists at {}",
            path.display()
        )));
    }

    let conn = open(path)?;
    conn.execute_batch(SCHEMA_SQL)?;

    conn.execute_batch(
        "INSERT OR IGNORE INTO vault_config(key,value,description) VALUES
         ('schema_version','1','OpenSessionLog schema version'),
         ('embedding_model',NULL,'User-supplied embedder model name (set in Phase 2)'),
         ('embedding_dimensions',NULL,'Embedding vector dimension (set in Phase 2)'),
         ('embedding_endpoint',NULL,'Remote embedding endpoint if any (set in Phase 2)'),
         ('default_distance_metric','cosine','sqlite-vec distance metric for semantic search');
         INSERT OR IGNORE INTO sources(name,is_active) VALUES
         ('claude',1),('codex',1),('copilot',1),('opencode',1);",
    )?;

    Ok(path.to_path_buf())
}

/// Return the default vault path.
/// If the `OSL_VAULT` environment variable is set, its value is used verbatim.
/// Otherwise returns `~/.opensessionlog/data.sqlite`. Phase 1 supports macOS/Linux only.
pub fn default_vault_path() -> PathBuf {
    if let Ok(env_path) = std::env::var("OSL_VAULT") {
        return PathBuf::from(env_path);
    }
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home)
        .join(".opensessionlog")
        .join("data.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_schema() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        init(path, true).unwrap();
        let conn = open(path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 9);
    }
}
