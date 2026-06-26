use std::path::PathBuf;

use osl::{db, embed, ingest, search};

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

#[test]
fn semantic_orders_by_cosine() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();
    ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();
    embed::run(&mut conn, &embedder(), None).unwrap();

    let conn = db::open(&vault).unwrap();
    let hits = search::semantic(&conn, "rust", 10).unwrap();
    assert!(!hits.is_empty());
    for window in hits.windows(2) {
        assert!(window[0].distance <= window[1].distance);
    }
}

#[test]
fn semantic_no_embeddings_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let conn = db::open(&vault).unwrap();
    let hits = search::semantic(&conn, "anything", 10).unwrap();
    assert!(hits.is_empty());
}
