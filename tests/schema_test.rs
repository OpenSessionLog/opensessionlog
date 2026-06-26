use std::path::PathBuf;

use rusqlite::Connection;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn init_tmp() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("vault.sqlite");
    osl::db::init(&path, true).unwrap();
    let conn = osl::db::open(&path).unwrap();
    (tmp, conn)
}

#[test]
fn schema_has_nine_base_tables_plus_fts() {
    let (_tmp, conn) = init_tmp();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(tables.contains(&"errata".to_string()));
    assert!(tables.contains(&"messages".to_string()));
    assert!(tables.contains(&"messages_fts".to_string()));
    assert!(tables.contains(&"projects".to_string()));
    assert!(tables.contains(&"reports".to_string()));
    assert!(tables.contains(&"sessions".to_string()));
    assert!(tables.contains(&"sources".to_string()));
    assert!(tables.contains(&"usage_summary".to_string()));
    assert!(tables.contains(&"vault_config".to_string()));

    let fts_kind: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(fts_kind.contains("USING fts5"));
}

#[test]
fn schema_has_expected_indexes_and_triggers() {
    let (_tmp, conn) = init_tmp();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('index','trigger') ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(names.contains(&"idx_messages_session".to_string()));
    assert!(names.contains(&"idx_sessions_project".to_string()));
    assert!(names.contains(&"idx_tool_calls_call_id".to_string()));
    assert!(names.contains(&"messages_ai".to_string()));
    assert!(names.contains(&"messages_ad".to_string()));
    assert!(names.contains(&"messages_au".to_string()));
}

#[test]
fn pragmas_are_set() {
    let (_tmp, conn) = init_tmp();
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
}

#[test]
fn seed_data_present() {
    let (_tmp, conn) = init_tmp();
    let schema_version: String = conn
        .query_row(
            "SELECT value FROM vault_config WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(schema_version, "1");

    let source_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(source_count, 4);
}

#[test]
fn messages_fts_is_populated_after_ingest() {
    let (_tmp, mut conn) = init_tmp();
    osl::ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}
