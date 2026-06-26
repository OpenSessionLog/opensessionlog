use std::path::PathBuf;

use osl::{db, embed, ingest, search};
use uuid::Uuid;

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
fn similar_returns_sibling_not_self() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();
    ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();
    embed::run(&mut conn, &embedder(), None).unwrap();

    let conn = db::open(&vault).unwrap();
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM sessions ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids.len(), 2);

    let a = Uuid::parse_str(&ids[0]).unwrap();
    let b = Uuid::parse_str(&ids[1]).unwrap();

    let a_hits = search::similar(&conn, &a, 10).unwrap();
    assert_eq!(a_hits.len(), 1);
    assert_eq!(a_hits[0].session_id, b);

    let b_hits = search::similar(&conn, &b, 10).unwrap();
    assert_eq!(b_hits.len(), 1);
    assert_eq!(b_hits[0].session_id, a);
}
