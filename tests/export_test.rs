use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn open_tmp() -> (tempfile::TempDir, rusqlite::Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("vault.sqlite");
    osl::db::init(&path, true).unwrap();
    let conn = osl::db::open(&path).unwrap();
    (tmp, conn)
}

#[test]
fn export_markdown_contains_session_and_messages() {
    let (_tmp, mut conn) = open_tmp();
    osl::ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();

    let session_id: String = conn
        .query_row("SELECT id FROM sessions LIMIT 1", [], |r| r.get(0))
        .unwrap();

    let md = osl::export::export_markdown(&conn, &session_id).unwrap();
    assert!(md.contains(&session_id));
    assert!(md.contains("Tool: Bash"));
    assert!(md.contains("mock output"));
}
