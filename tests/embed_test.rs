use std::path::PathBuf;

use osl::{db, embed, ingest};
use rusqlite::OptionalExtension;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn embedder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("embed")
        .join("identity.py")
}

fn config_value(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM vault_config WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
    .flatten()
}

#[test]
fn embed_populates_messages_and_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();

    let stats = embed::run(&mut conn, &embedder(), Some(5)).unwrap();
    assert!(stats.messages_embedded > 0);
    assert!(stats.sessions_summarized > 0);

    let conn = db::open(&vault).unwrap();
    let mut stmt = conn
        .prepare("SELECT embedding FROM messages WHERE embedding IS NOT NULL")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut count = 0;
    while let Some(row) = rows.next().unwrap() {
        let blob: Vec<u8> = row.get(0).unwrap();
        assert_eq!(blob.len() % 4, 0);
        assert_eq!(blob.len(), 8 * 4);
        count += 1;
    }
    assert_eq!(count, stats.messages_embedded as usize);

    assert_eq!(
        config_value(&conn, "embedding_model").as_deref(),
        Some("identity-fixture")
    );
    assert_eq!(
        config_value(&conn, "embedding_dimensions").as_deref(),
        Some("8")
    );
    assert!(config_value(&conn, "embedder_path").is_some());

    let summaries: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE summary_embedding IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summaries, stats.sessions_summarized as i64);
}

#[test]
fn embed_upserts_config_on_upgraded_vault() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();
    conn.execute(
        "DELETE FROM vault_config WHERE key IN ('embedding_model','embedding_dimensions','embedder_path')",
        [],
    )
    .unwrap();

    let stats = embed::run(&mut conn, &embedder(), Some(5)).unwrap();
    assert!(stats.messages_embedded > 0);

    let conn = db::open(&vault).unwrap();
    assert!(config_value(&conn, "embedder_path").is_some());
    assert_eq!(
        config_value(&conn, "embedding_dimensions").as_deref(),
        Some("8")
    );
}

#[test]
fn embed_header_only_is_noop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    let stats = embed::run(&mut conn, &embedder(), None).unwrap();
    assert_eq!(stats.messages_embedded, 0);
    assert_eq!(stats.sessions_summarized, 0);

    let conn = db::open(&vault).unwrap();
    assert_eq!(
        config_value(&conn, "embedding_model").as_deref(),
        Some("identity-fixture")
    );
    assert_eq!(
        config_value(&conn, "embedding_dimensions").as_deref(),
        Some("8")
    );
    assert!(config_value(&conn, "embedder_path").is_some());
}
