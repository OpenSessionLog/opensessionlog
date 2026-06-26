use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use rusqlite::Connection;

/// Build a synthetic Hermes SQLite database at the given path.
fn build_hermes_db(path: &PathBuf) {
    let conn = Connection::open(path).unwrap();

    conn.execute_batch(
        "
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            user_id TEXT,
            model TEXT,
            model_config TEXT,
            system_prompt TEXT,
            parent_session_id TEXT,
            started_at REAL NOT NULL,
            ended_at REAL,
            end_reason TEXT,
            message_count INTEGER DEFAULT 0,
            tool_call_count INTEGER DEFAULT 0,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0,
            cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0,
            billing_provider TEXT,
            billing_base_url TEXT,
            billing_mode TEXT,
            estimated_cost_usd REAL,
            actual_cost_usd REAL,
            cost_status TEXT,
            cost_source TEXT,
            pricing_version TEXT,
            title TEXT,
            api_call_count INTEGER DEFAULT 0,
            handoff_state TEXT,
            handoff_platform TEXT,
            handoff_error TEXT,
            cwd TEXT,
            rewind_count INTEGER NOT NULL DEFAULT 0,
            archived INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
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

    let sid = "hermes_integration_test";
    let child_sid = "hermes_integration_test_child";
    let started = 1780258945.0_f64;
    let ended = 1780258960.0_f64;

    conn.execute(
        "INSERT INTO sessions (
            id, source, model, started_at, ended_at,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            tool_call_count, title, cwd, archived
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)",
        rusqlite::params![
            sid,
            "telegram",
            "claude-sonnet-4-5",
            started,
            ended,
            500_i64,
            200_i64,
            1000_i64,
            0_i64,
            1_i64,
            "Hermes integration test",
            Option::<&str>::None,
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (
            id, source, parent_session_id, model, started_at, ended_at,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            tool_call_count, title, cwd, archived
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)",
        rusqlite::params![
            child_sid,
            "telegram",
            sid,
            "claude-sonnet-4-5",
            started + 1.0,
            ended + 1.0,
            50_i64,
            20_i64,
            0_i64,
            0_i64,
            0_i64,
            "Hermes integration test child",
            Option::<&str>::None,
        ],
    )
    .unwrap();

    let insert_message = |role: &str,
                          content: Option<&str>,
                          tool_calls: Option<&str>,
                          tool_call_id: Option<&str>,
                          reasoning_content: Option<&str>,
                          timestamp: f64| {
        conn.execute(
            "INSERT INTO messages (
                session_id, role, content, tool_calls, tool_call_id,
                reasoning_content, timestamp
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                sid,
                role,
                content,
                tool_calls,
                tool_call_id,
                reasoning_content,
                timestamp,
            ],
        )
        .unwrap();
    };

    insert_message(
        "user",
        Some("What time is it?"),
        None,
        None,
        None,
        1780258945.5,
    );
    insert_message(
        "assistant",
        Some(""),
        Some(
            r#"[{"id":"call_int_01","call_id":"call_int_01","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"date\"}"}}]"#,
        ),
        None,
        Some("I need to check the system time."),
        1780258946.0,
    );
    insert_message(
        "tool",
        Some(r#"{"output": "Fri Jun 26 14:02:25 UTC 2026", "exit_code": 0, "error": null}"#),
        None,
        Some("call_int_01"),
        None,
        1780258947.0,
    );
    insert_message(
        "assistant",
        Some("The current time is Fri Jun 26 14:02:25 UTC 2026."),
        None,
        None,
        None,
        1780258948.0,
    );

    conn.execute(
        "INSERT INTO messages (
            session_id, role, content, tool_calls, tool_call_id,
            reasoning_content, timestamp
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            child_sid,
            "user",
            Some("Child question?"),
            Option::<&str>::None,
            Option::<&str>::None,
            Option::<&str>::None,
            started + 1.5,
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (
            session_id, role, content, tool_calls, tool_call_id,
            reasoning_content, timestamp
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            child_sid,
            "assistant",
            Some("Child answer."),
            Option::<&str>::None,
            Option::<&str>::None,
            Option::<&str>::None,
            started + 2.0,
        ],
    )
    .unwrap();
}

fn osl() -> Command {
    Command::cargo_bin("osl").unwrap()
}

#[test]
fn hermes_ingest_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("vault.sqlite");
    let hermes_db = tmp.path().join("hermes.db");

    build_hermes_db(&hermes_db);

    // Init vault
    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success()
        .stdout(contains("initialized vault"));

    // Ingest the Hermes database
    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &hermes_db.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("ingested"))
        .stdout(contains("Hermes integration test"));

    // Grep for message content
    let grep = osl()
        .args(["--vault", &vault.to_string_lossy(), "grep", "current time"])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("current time"));

    // Extract session id from grep output: `[<uuid>] user: ...`
    let session_id = stdout
        .split(']')
        .next()
        .unwrap()
        .trim_start_matches('[')
        .trim();
    assert!(!session_id.is_empty(), "grep should return a session id");

    // Export the session
    let export = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "export",
            session_id,
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(export.status.success(), "export should succeed");
    let export_out = String::from_utf8_lossy(&export.stdout);
    assert!(
        export_out.contains("hermes"),
        "export should mention hermes source, got: {export_out}"
    );
    assert!(
        export_out.contains("claude-sonnet-4-5"),
        "export should contain model"
    );
    assert!(
        export_out.contains("terminal"),
        "export should contain tool name"
    );
    assert!(
        export_out.contains("The current time is"),
        "export should contain assistant response"
    );
}

#[test]
fn hermes_reingest_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("vault.sqlite");
    let hermes_db = tmp.path().join("hermes.db");

    build_hermes_db(&hermes_db);

    // Init vault
    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    // First ingest
    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &hermes_db.to_string_lossy(),
        ])
        .assert()
        .success();

    // Second ingest (should be idempotent)
    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &hermes_db.to_string_lossy(),
        ])
        .assert()
        .success();

    // Grep should still find content and not duplicate sessions
    let grep = osl()
        .args(["--vault", &vault.to_string_lossy(), "grep", "current time"])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("current time"));

    let unique_sessions: std::collections::HashSet<&str> = stdout
        .lines()
        .filter(|l| l.starts_with('['))
        .filter_map(|l| l.split(']').next())
        .map(|s| s.trim_start_matches('[').trim())
        .collect();
    assert_eq!(
        unique_sessions.len(),
        1,
        "expected exactly 1 unique session after re-ingest, got {}: {stdout}",
        unique_sessions.len()
    );
}
