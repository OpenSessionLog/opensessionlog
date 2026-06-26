use std::path::PathBuf;

use rusqlite::Connection;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn open_tmp() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("vault.sqlite");
    osl::db::init(&path, true).unwrap();
    let conn = osl::db::open(&path).unwrap();
    (tmp, conn)
}

fn row_counts(conn: &Connection) -> (i64, i64, i64) {
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let tool_calls: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    (sessions, messages, tool_calls)
}

#[test]
fn re_ingest_same_file_is_idempotent() {
    let (_tmp, mut conn) = open_tmp();
    let path = fixture("with_tool_call.jsonl");

    osl::ingest::ingest(&mut conn, &path).unwrap();
    let first = row_counts(&conn);

    osl::ingest::ingest(&mut conn, &path).unwrap();
    let second = row_counts(&conn);

    assert_eq!(first, second);
}

#[test]
fn same_session_at_different_paths_has_identical_session_id() {
    let (tmp, mut conn) = open_tmp();
    let original = fixture("minimal.jsonl");
    let copy = tmp.path().join("minimal-copy.jsonl");
    std::fs::copy(&original, &copy).unwrap();

    osl::ingest::ingest(&mut conn, &original).unwrap();
    osl::ingest::ingest(&mut conn, &copy).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(DISTINCT id) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
}
