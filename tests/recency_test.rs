use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use osl::{db, embed, ingest, recency};

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

fn set_mtime(path: &std::path::Path, secs: i64) {
    let f = File::open(path).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
        .unwrap();
}

#[test]
fn ingest_recency_30_skips_old_jsonl() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let fixture_path = fixture("minimal.jsonl");
    let now = recency::now_unix_seconds();

    // Old file (60 days) with recency 30 → skipped, empty report.
    set_mtime(&fixture_path, now - 60 * 86400);
    let mut conn = db::open(&vault).unwrap();
    let filter = recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
    let report = ingest::ingest_filtered(&mut conn, &fixture_path, &filter).unwrap();
    assert!(report.sessions.is_empty());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);

    // Recent file (today) with recency 30 → ingested.
    set_mtime(&fixture_path, now);
    let mut conn = db::open(&vault).unwrap();
    let report = ingest::ingest_filtered(&mut conn, &fixture_path, &filter).unwrap();
    assert_eq!(report.sessions.len(), 1);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    // No filter always ingests regardless of mtime.
    set_mtime(&fixture_path, now - 60 * 86400);
    let mut conn = db::open(&vault).unwrap();
    let report =
        ingest::ingest_filtered(&mut conn, &fixture_path, &recency::RecencyFilter::none()).unwrap();
    // Same session idempotent re-ingest; count stays 1.
    assert_eq!(report.sessions.len(), 1);
}

#[test]
fn ingest_since_date_includes_today() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let fixture_path = fixture("minimal.jsonl");
    let now = recency::now_unix_seconds();
    set_mtime(&fixture_path, now);

    let today = recency::today_ymd(now);
    let filter = recency::RecencyFilter::from_flags(None, Some(today), now).unwrap();

    let mut conn = db::open(&vault).unwrap();
    let report = ingest::ingest_filtered(&mut conn, &fixture_path, &filter).unwrap();
    assert_eq!(report.sessions.len(), 1);
}

#[test]
fn embed_recency_filter_only_embeds_recent_messages() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();

    // Count total messages.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert!(total >= 2, "fixture should have at least 2 messages");

    // Mark all messages old except the last one.
    let old = "2020-01-01T00:00:00Z";
    conn.execute(
        "UPDATE messages SET created_at = ?1 WHERE id < (SELECT MAX(id) FROM messages)",
        [old],
    )
    .unwrap();

    let now = recency::now_unix_seconds();
    let filter = recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
    let stats = embed::run_with_filter(&mut conn, &embedder(), None, &filter, false).unwrap();
    assert_eq!(stats.messages_embedded, 1);

    // Running with no filter and force=false embeds the remaining NULL messages.
    let stats2 = embed::run_with_filter(
        &mut conn,
        &embedder(),
        None,
        &recency::RecencyFilter::none(),
        false,
    )
    .unwrap();
    assert_eq!(stats2.messages_embedded, (total - 1) as u64);
}

#[test]
fn embed_force_reembeds_all_in_scope() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("minimal.jsonl")).unwrap();

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();

    // First embed — normal incremental.
    let stats1 = embed::run(&mut conn, &embedder(), None).unwrap();
    assert_eq!(stats1.messages_embedded, total as u64);

    // Force re-embed all (no recency filter).
    let stats2 = embed::run_with_filter(
        &mut conn,
        &embedder(),
        None,
        &recency::RecencyFilter::none(),
        true,
    )
    .unwrap();
    assert_eq!(stats2.messages_embedded, total as u64);
}

#[test]
fn embed_force_with_recency_only_reembeds_window() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    db::init(&vault, false).unwrap();

    let mut conn = db::open(&vault).unwrap();
    ingest::ingest(&mut conn, &fixture("with_tool_call.jsonl")).unwrap();

    // Embed everything first.
    embed::run(&mut conn, &embedder(), None).unwrap();

    // Now mark all but one message old.
    let old = "2020-01-01T00:00:00Z";
    conn.execute(
        "UPDATE messages SET created_at = ?1 WHERE id < (SELECT MAX(id) FROM messages)",
        [old],
    )
    .unwrap();

    let now = recency::now_unix_seconds();
    let filter = recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
    let stats = embed::run_with_filter(&mut conn, &embedder(), None, &filter, true).unwrap();
    assert_eq!(stats.messages_embedded, 1);
}

#[test]
fn ingest_sqlite_source_recency_filters_started_at() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    let hermes_db = tmp.path().join("hermes.db");
    db::init(&vault, false).unwrap();

    let conn = rusqlite::Connection::open(&hermes_db).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            model TEXT,
            parent_session_id TEXT,
            started_at REAL NOT NULL,
            ended_at REAL,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0,
            cache_write_tokens INTEGER DEFAULT 0,
            title TEXT,
            cwd TEXT,
            archived INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL,
            token_count INTEGER,
            finish_reason TEXT,
            reasoning TEXT,
            reasoning_content TEXT,
            reasoning_details TEXT,
            codex_reasoning_items TEXT,
            codex_message_items TEXT,
            platform_message_id TEXT,
            observed INTEGER DEFAULT 0,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE compression_locks (
            session_id TEXT PRIMARY KEY,
            holder TEXT NOT NULL,
            acquired_at REAL NOT NULL,
            expires_at REAL NOT NULL
        );
        CREATE TABLE schema_version (version INTEGER NOT NULL);
        CREATE TABLE state_meta (key TEXT PRIMARY KEY, value TEXT);
        ",
    )
    .unwrap();

    let now = recency::now_unix_seconds();
    let old_started = (now - 60 * 86400) as f64;
    let recent_started = now as f64;

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at, title, archived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        rusqlite::params![
            "old_ses",
            "telegram",
            "claude",
            old_started,
            old_started + 10.0,
            "Old"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at, title, archived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        rusqlite::params![
            "recent_ses",
            "telegram",
            "claude",
            recent_started,
            recent_started + 10.0,
            "Recent"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'hello', ?2)",
        rusqlite::params!["recent_ses", recent_started + 1.0],
    )
    .unwrap();
    drop(conn);

    let mut conn = db::open(&vault).unwrap();
    let filter = recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
    let report = ingest::ingest_filtered(&mut conn, &hermes_db, &filter).unwrap();
    assert_eq!(report.sessions.len(), 1);
    assert_eq!(
        report.sessions[0].title.as_deref(),
        Some("Recent"),
        "expected only the recent session"
    );
}
