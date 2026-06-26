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
fn grep_finds_message_content() {
    let (_tmp, mut conn) = open_tmp();
    osl::ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();

    let hits = osl::search::grep(&conn, "Claude", 20, None).unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().any(|h| h.content_snippet.contains("Claude")));
}

#[test]
fn grep_finds_synthesized_tool_output() {
    let (_tmp, mut conn) = open_tmp();
    osl::ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();

    let hits = osl::search::grep(&conn, "mock output", 20, None).unwrap();
    assert!(!hits.is_empty(), "expected to find synthesized tool output");
}
