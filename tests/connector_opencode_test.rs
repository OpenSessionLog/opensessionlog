use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use rusqlite::Connection;

/// Build a synthetic OpenCode SQLite database at the given path.
fn build_opencode_db(path: &PathBuf) {
    let conn = Connection::open(path).unwrap();

    conn.execute_batch(
        "
        CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT,
            parent_id TEXT,
            slug TEXT,
            directory TEXT,
            title TEXT,
            version TEXT,
            share_url TEXT,
            summary_additions INTEGER,
            summary_deletions INTEGER,
            summary_files INTEGER,
            summary_diffs TEXT,
            revert TEXT,
            permission TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            time_compacting INTEGER,
            time_archived INTEGER,
            workspace_id TEXT,
            path TEXT,
            agent TEXT,
            model TEXT,
            cost REAL,
            tokens_input INTEGER,
            tokens_output INTEGER,
            tokens_reasoning INTEGER,
            tokens_cache_read INTEGER,
            tokens_cache_write INTEGER,
            metadata TEXT
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            data TEXT
        );
        CREATE TABLE part (
            id TEXT PRIMARY KEY,
            message_id TEXT,
            session_id TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            data TEXT
        );
        CREATE TABLE project (
            id TEXT PRIMARY KEY,
            worktree TEXT,
            vcs TEXT,
            name TEXT,
            icon_url TEXT,
            icon_color TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            time_initialized INTEGER,
            sandboxes TEXT,
            commands TEXT,
            icon_url_override TEXT
        );
        CREATE TABLE session_message (
            id TEXT PRIMARY KEY,
            session_id TEXT,
            type TEXT,
            time_created INTEGER,
            time_updated INTEGER,
            data TEXT,
            seq INTEGER
        );
        ",
    )
    .unwrap();

    let now_ms = 1780251900000i64;
    let sid = "ses_integration_test";

    // Session row
    conn.execute(
        "INSERT INTO session (id, title, directory, time_created, time_updated,
         tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
         cost, model, agent, version)
         VALUES (?1, 'Integration test session', '/tmp', ?2, ?3, 500, 200, 50, 1000, 0, 0.0,
         '{\"id\":\"test-model\",\"providerID\":\"opencode\"}', 'build', 'local')",
        rusqlite::params![sid, now_ms, now_ms + 60000],
    )
    .unwrap();

    // User message
    let msg1 = format!("msg_{}_user", sid);
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        rusqlite::params![
            msg1,
            sid,
            now_ms + 1000,
            r#"{"role":"user","time":{"created":1780251901000},"agent":"build","model":{"providerID":"opencode","modelID":"test-model"}}"#
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            format!("prt_{}_text1", sid),
            msg1,
            sid,
            now_ms + 1000,
            now_ms + 1000,
            r#"{"type":"text","text":"Run the integration tests"}"#
        ],
    )
    .unwrap();

    // Assistant message with reasoning + text
    let msg2 = format!("msg_{}_assistant", sid);
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        rusqlite::params![
            msg2,
            sid,
            now_ms + 5000,
            format!(
                r#"{{"parentID":"{}","role":"assistant","mode":"build","agent":"build","cost":0,"tokens":{{"total":700,"input":500,"output":200,"reasoning":50,"cache":{{"write":0,"read":1000}}}},"modelID":"test-model","providerID":"opencode","time":{{"created":{},"completed":{}}},"finish":"stop"}}"#,
                msg1, now_ms + 5000, now_ms + 55000
            )
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            format!("prt_{}_reason1", sid),
            msg2,
            sid,
            now_ms + 5000,
            now_ms + 5000,
            r#"{"type":"reasoning","text":"The user wants to run integration tests. I need to execute the test command and report results.","time":{"start":1780251905000,"end":1780251907000}}"#
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            format!("prt_{}_tool1", sid),
            msg2,
            sid,
            now_ms + 6000,
            now_ms + 6000,
            format!(
                r#"{{"type":"tool","tool":"bash","callID":"call_00_integration","state":{{"status":"completed","input":{{"command":"cargo test","description":"Run tests","timeout":300000}},"output":"PASS all integration tests","metadata":{{"exit":0,"truncated":false}},"time":{{"start":{},"end":{}}}}}}}"#,
                now_ms + 7000, now_ms + 50000
            )
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            format!("prt_{}_text2", sid),
            msg2,
            sid,
            now_ms + 51000,
            now_ms + 51000,
            r#"{"type":"text","text":"All integration tests passed!"}"#
        ],
    )
    .unwrap();
}

fn osl() -> Command {
    Command::cargo_bin("osl").unwrap()
}

#[test]
fn opencode_ingest_via_cli() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("vault.sqlite");
    let opencode_db = tmp.path().join("opencode.db");

    build_opencode_db(&opencode_db);

    // Init vault
    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success()
        .stdout(contains("initialized vault"));

    // Ingest the opencode database
    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &opencode_db.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("ingested"))
        .stdout(contains("Integration test session"));

    // Grep for message content
    let grep = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "grep",
            "integration tests",
        ])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("integration tests"));

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
        export_out.contains("opencode"),
        "export should mention opencode source, got: {export_out}"
    );
    assert!(
        export_out.contains("test-model"),
        "export should contain model"
    );
    assert!(export_out.contains("bash"), "export should contain tool");
    assert!(
        export_out.contains("All integration tests passed"),
        "export should contain assistant response"
    );
}

#[test]
fn opencode_reingest_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("vault.sqlite");
    let opencode_db = tmp.path().join("opencode.db");

    build_opencode_db(&opencode_db);

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
            &opencode_db.to_string_lossy(),
        ])
        .assert()
        .success();

    // Second ingest (should be idempotent)
    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &opencode_db.to_string_lossy(),
        ])
        .assert()
        .success();

    // Grep should still find content (message text, not tool output) and not duplicate
    let grep = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "grep",
            "integration tests",
        ])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let stdout = String::from_utf8_lossy(&grep.stdout);
    // Should find the message content
    assert!(stdout.contains("integration tests"));
    // Extract unique session IDs from each result line: [<uuid>] ...
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
