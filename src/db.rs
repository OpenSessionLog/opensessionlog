use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{OslError, Result};

pub const SCHEMA_SQL: &str = include_str!("schema.sql");

fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let mut rows = stmt.query([table])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        if name == col {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Open a SQLite connection with the runtime pragmas required by OpenSessionLog.
/// WAL mode, foreign keys, and normal synchronous mode are set on every connection.
pub fn open(path: &Path) -> Result<Connection> {
    crate::vec::init();

    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )?;

    // Lightweight migration: pre-Issue #5 vaults may be missing the 'opencode' source.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO sources(name,is_active)
         SELECT 'opencode', 1
         WHERE EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='sources')",
        [],
    );

    // Migration: add the 'hermes' source to existing vaults.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO sources(name,is_active)
         SELECT 'hermes', 1
         WHERE EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='sources')",
        [],
    );

    // Phase 2 migration: add sessions.summary_embedding to pre-existing vaults.
    let sessions_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sessions')",
        [],
        |r| r.get(0),
    )?;
    if sessions_exists && !column_exists(&conn, "sessions", "summary_embedding")? {
        conn.execute("ALTER TABLE sessions ADD COLUMN summary_embedding BLOB", [])?;
    }

    // Phase 3 migration: create reports + usage_summary in pre-Phase-3 vaults.
    let needs_reports: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='reports')",
        [],
        |r| r.get(0),
    )?;
    let needs_usage: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='usage_summary')",
        [],
        |r| r.get(0),
    )?;
    if needs_reports && needs_usage {
        // already migrated — fast path, do nothing
    } else {
        // Re-run the DDL fragments verbatim from schema.sql. These are CREATE ... IF NOT EXISTS,
        // so re-running on a fresh Phase 3 vault is a no-op.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reports (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                period_start TEXT NOT NULL,
                period_end TEXT NOT NULL,
                generated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                data_json TEXT NOT NULL,
                markdown TEXT,
                previous_report_id INTEGER REFERENCES reports(id),
                token_budget_used INTEGER,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE INDEX IF NOT EXISTS idx_reports_scope ON reports(scope);
             CREATE INDEX IF NOT EXISTS idx_reports_period ON reports(period_start, period_end);
             CREATE INDEX IF NOT EXISTS idx_reports_previous ON reports(previous_report_id);

             CREATE TABLE IF NOT EXISTS usage_summary (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                date TEXT NOT NULL,
                source_id INTEGER NOT NULL REFERENCES sources(id),
                project_id INTEGER REFERENCES projects(id),
                session_count INTEGER NOT NULL DEFAULT 0,
                message_count INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER GENERATED ALWAYS AS (input_tokens + output_tokens + cache_read_tokens + cache_write_tokens) STORED,
                estimated_cost_usd REAL NOT NULL DEFAULT 0,
                tool_call_count INTEGER NOT NULL DEFAULT 0,
                error_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                UNIQUE(date, source_id, project_id)
             );
             CREATE INDEX IF NOT EXISTS idx_usage_date ON usage_summary(date);
             CREATE INDEX IF NOT EXISTS idx_usage_source ON usage_summary(source_id);
             CREATE INDEX IF NOT EXISTS idx_usage_project ON usage_summary(project_id);",
        )?;
    }

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
         ('embedder_path',NULL,'Absolute path to the user-supplied embedder script (set by osl embed)'),
         ('default_distance_metric','cosine','sqlite-vec distance metric for semantic search');
         INSERT OR IGNORE INTO sources(name,is_active) VALUES
         ('claude',1),('codex',1),('copilot',1),('opencode',1),('hermes',1);",
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

    #[test]
    fn open_migrates_existing_vault_adds_summary_embedding() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        init(path, true).unwrap();

        open(path).unwrap();
        let conn = open(path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='summary_embedding'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 1);

        // Idempotent: second open must not error.
        open(path).unwrap();
    }

    #[test]
    fn open_migrates_old_schema_vault_re_adds_summary_embedding() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        init(path, true).unwrap();

        let conn = open(path).unwrap();
        conn.execute("ALTER TABLE sessions DROP COLUMN summary_embedding", [])
            .unwrap();
        assert!(!column_exists(&conn, "sessions", "summary_embedding").unwrap());

        open(path).unwrap();
        let conn = open(path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='summary_embedding'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 1);
    }

    #[test]
    fn init_succeeds_on_fresh_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("data.sqlite");
        init(&path, false).unwrap();

        let conn = open(&path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='summary_embedding'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 1);
    }

    #[test]
    fn open_migrates_phase3_tables_to_old_vault() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("data.sqlite");
        init(&path, true).unwrap();

        let conn = open(&path).unwrap();
        conn.execute("DROP TABLE IF EXISTS reports", []).unwrap();
        conn.execute("DROP TABLE IF EXISTS usage_summary", [])
            .unwrap();

        open(&path).unwrap();
        let conn = open(&path).unwrap();
        let reports: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='reports')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let usage: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='usage_summary')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(reports != 0);
        assert!(usage != 0);

        // Idempotent: second open must not error.
        open(&path).unwrap();
    }
}
