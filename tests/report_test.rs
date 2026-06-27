use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use assert_cmd::Command;
use predicates::str::contains;
use uuid::Uuid;

use osl::error::OslError;
use osl::model::ReportPeriodKind;

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn new_test_uuid() -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("osl-report-test-{n}").as_bytes(),
    )
    .to_string()
}

fn open_tmp() -> (tempfile::TempDir, rusqlite::Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("vault.sqlite");
    osl::db::init(&path, true).unwrap();
    let conn = osl::db::open(&path).unwrap();
    (tmp, conn)
}

fn osl() -> Command {
    Command::cargo_bin("osl").unwrap()
}

#[allow(clippy::too_many_arguments)]
fn insert_session(
    conn: &rusqlite::Connection,
    started: &str,
    ended: Option<&str>,
    source: &str,
    project_slug: Option<&str>,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_write: i64,
    cost_usd: Option<f64>,
    msg_roles: &[&str],
    tool_names: &[&str],
    error_count: i64,
) -> String {
    let source_id: i64 = conn
        .query_row("SELECT id FROM sources WHERE name = ?1", [source], |r| {
            r.get(0)
        })
        .unwrap();

    let project_id: Option<i64> = project_slug.map(|slug| {
        conn.execute(
            "INSERT OR IGNORE INTO projects(root_path, slug) VALUES (?1, ?2)",
            [format!("/tmp/{slug}"), slug.to_string()],
        )
        .unwrap();
        conn.query_row("SELECT id FROM projects WHERE slug = ?1", [slug], |r| {
            r.get(0)
        })
        .unwrap()
    });

    let session_id = new_test_uuid();
    conn.execute(
        "INSERT INTO sessions (
            id, source_id, project_id, started_at, ended_at, duration_seconds,
            model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            estimated_cost_usd, error_count
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        [
            &session_id as &dyn rusqlite::ToSql,
            &source_id as &dyn rusqlite::ToSql,
            &project_id as &dyn rusqlite::ToSql,
            &Some(started) as &dyn rusqlite::ToSql,
            &ended as &dyn rusqlite::ToSql,
            &None::<i64> as &dyn rusqlite::ToSql,
            &Some(model) as &dyn rusqlite::ToSql,
            &input_tokens as &dyn rusqlite::ToSql,
            &output_tokens as &dyn rusqlite::ToSql,
            &cache_read as &dyn rusqlite::ToSql,
            &cache_write as &dyn rusqlite::ToSql,
            &cost_usd as &dyn rusqlite::ToSql,
            &error_count as &dyn rusqlite::ToSql,
        ],
    )
    .unwrap();

    for role in msg_roles {
        let msg_id = new_test_uuid();
        conn.execute(
            "INSERT INTO messages(uuid, session_id, role, sequence) VALUES (?1, ?2, ?3, 0)",
            [
                &msg_id as &dyn rusqlite::ToSql,
                &session_id as &dyn rusqlite::ToSql,
                &role as &dyn rusqlite::ToSql,
            ],
        )
        .unwrap();
    }

    for tool_name in tool_names {
        let tc_id = new_test_uuid();
        conn.execute(
            "INSERT INTO tool_calls(uuid, session_id, tool_name) VALUES (?1, ?2, ?3)",
            [
                &tc_id as &dyn rusqlite::ToSql,
                &session_id as &dyn rusqlite::ToSql,
                &tool_name as &dyn rusqlite::ToSql,
            ],
        )
        .unwrap();
    }

    session_id
}

fn date_offset(conn: &rusqlite::Connection, modifier: &str) -> String {
    conn.query_row(&format!("SELECT date('now','{modifier}')"), [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn today_iso_date(conn: &rusqlite::Connection) -> String {
    conn.query_row("SELECT date('now')", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn report_empty_vault_prints_no_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
        ])
        .assert()
        .success()
        .stdout(contains("no sessions found"))
        .stderr("");
}

#[test]
fn report_daily_aggregates_correctly() {
    let (_tmp, mut conn) = open_tmp();
    let day = today_iso_date(&conn);
    let started = format!("{day}T10:00:00Z");

    insert_session(
        &conn,
        &started,
        None,
        "claude",
        Some("alpha"),
        "claude-sonnet-4",
        100,
        50,
        10,
        5,
        Some(0.001),
        &["user", "assistant", "user", "assistant", "user"],
        &["Bash", "Read"],
        1,
    );
    insert_session(
        &conn,
        &started,
        None,
        "claude",
        Some("alpha"),
        "claude-sonnet-4",
        100,
        50,
        10,
        5,
        Some(0.001),
        &[],
        &[],
        0,
    );
    insert_session(
        &conn,
        &started,
        None,
        "claude",
        Some("alpha"),
        "claude-sonnet-4",
        100,
        50,
        10,
        5,
        Some(0.001),
        &[],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 3);
    assert_eq!(doc.metrics.total_tokens, 3 * (100 + 50 + 10 + 5));
    assert_eq!(doc.metrics.message_count, 5);
    assert_eq!(doc.metrics.tool_call_count, 2);
    assert_eq!(doc.metrics.error_count, 1);

    let role_sum: i64 = doc.metrics.messages_by_role.iter().map(|r| r.count).sum();
    assert_eq!(role_sum, 5);

    let tool_names: Vec<&str> = doc
        .metrics
        .top_tools
        .iter()
        .map(|t| t.tool_name.as_str())
        .collect();
    assert!(tool_names.contains(&"Bash"));
}

#[test]
fn report_daily_breakdown_groups_by_date() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-01T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-10T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.daily_breakdown.len(), 3);
    let dates: Vec<&str> = doc
        .metrics
        .daily_breakdown
        .iter()
        .map(|d| d.date.as_str())
        .collect();
    assert_eq!(dates, vec!["2026-06-01", "2026-06-05", "2026-06-10"]);
}

#[test]
fn report_last_30_days_rolling_window() {
    let (_tmp, mut conn) = open_tmp();
    let outside = date_offset(&conn, "-40 days");
    let inside = date_offset(&conn, "-10 days");

    insert_session(
        &conn,
        &format!("{outside}T10:00:00Z"),
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        &format!("{inside}T10:00:00Z"),
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Last30Days),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 1);
}

#[test]
fn report_custom_from_to_range() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-01T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-10T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-20T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-15"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 2);
}

#[test]
fn report_custom_from_gt_to_errors() {
    let (_tmp, mut conn) = open_tmp();
    let err = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-10"),
        Some("2026-06-01"),
        None,
        None,
        false,
    )
    .unwrap_err();

    match err {
        OslError::Usage(msg) => assert!(msg.contains("must not be after")),
        other => panic!("expected Usage error, got {other:?}"),
    }
}

#[test]
fn report_period_and_from_mutually_exclusive() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--from",
            "2026-06-01",
            "--to",
            "2026-06-15",
        ])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn report_project_filter() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("beta"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        Some("alpha"),
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 2);
}

#[test]
fn report_source_filter() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "codex",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        Some("codex"),
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 1);
}

#[test]
fn report_format_json_valid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();
    {
        let conn = osl::db::open(&vault).unwrap();
        let day = today_iso_date(&conn);
        insert_session(
            &conn,
            &format!("{day}T10:00:00Z"),
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    let out = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["period_kind"], "daily");
    assert_eq!(json["metrics"]["total_sessions"], 1);
    assert_eq!(json["from_cache"], false);
}

#[test]
fn report_no_data_cost_renders_no_data() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert!(doc.metrics.estimated_cost_usd.is_none());
    let md = osl::report::render_markdown(&doc);
    assert!(md.contains("no data"));
}

#[test]
fn report_save_caches_closed_period() {
    let (_tmp, mut conn) = open_tmp();
    let start = date_offset(&conn, "-12 days");
    let end = date_offset(&conn, "-5 days");
    let session_day = date_offset(&conn, "-10 days");

    insert_session(
        &conn,
        &format!("{session_day}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let first = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some(&start),
        Some(&end),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.metrics.total_sessions, 1);

    let second = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some(&start),
        Some(&end),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(second.from_cache);
    assert_eq!(second.metrics.total_sessions, 1);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reports WHERE scope='global' AND period_start=?1 AND period_end=?2",
            [&start, &end],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn report_save_replaces_open_period() {
    let (_tmp, mut conn) = open_tmp();
    let day = today_iso_date(&conn);

    insert_session(
        &conn,
        &format!("{day}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let first = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.metrics.total_sessions, 1);

    insert_session(
        &conn,
        &format!("{day}T11:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let second = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    assert!(!second.from_cache);
    assert_eq!(second.metrics.total_sessions, 2);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reports WHERE scope='global' AND period_start=?1 AND period_end=?2",
            [&day, &day],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn report_save_last_30_days_always_replaces() {
    let (_tmp, mut conn) = open_tmp();
    let day = today_iso_date(&conn);

    insert_session(
        &conn,
        &format!("{day}T09:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let first = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Last30Days),
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.metrics.total_sessions, 1);

    insert_session(
        &conn,
        &format!("{day}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let second = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Last30Days),
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();
    assert!(!second.from_cache);
    assert_eq!(second.metrics.total_sessions, 2);

    let start = date_offset(&conn, "-29 days");
    let end = today_iso_date(&conn);
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reports WHERE scope='global' AND period_start=?1 AND period_end=?2",
            [&start, &end],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn report_usage_summary_populated_after_run() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-01T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        20,
        20,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM usage_summary", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);

    let total: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_summary",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(total, (10 + 10) + (20 + 20));
}

#[test]
fn report_cli_markdown_pipes_cleanly() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();
    {
        let conn = osl::db::open(&vault).unwrap();
        let day = today_iso_date(&conn);
        insert_session(
            &conn,
            &format!("{day}T10:00:00Z"),
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    let out = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# Usage Report"));
}

#[test]
fn report_avg_session_duration() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-01T10:00:00Z",
        Some("2026-06-01T10:02:30Z"),
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-01"),
        None,
        None,
        false,
    )
    .unwrap();

    let avg = doc.metrics.avg_session_duration_seconds.unwrap();
    assert!((avg - 150.0).abs() < 0.5);
}

#[test]
fn report_unique_models() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "claude-sonnet-4",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "claude-sonnet-4",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "gpt-4o",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.unique_models, 2);
}

#[test]
fn report_top_projects_orders_by_session_count() {
    let (_tmp, mut conn) = open_tmp();
    for _ in 0..5 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            Some("alpha"),
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }
    for _ in 0..2 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            Some("beta"),
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.top_projects[0].slug, Some("alpha".to_string()));
    assert_eq!(doc.metrics.top_projects[0].session_count, 5);
    assert_eq!(doc.metrics.top_projects[1].slug, Some("beta".to_string()));
    assert_eq!(doc.metrics.top_projects[1].session_count, 2);
}

#[test]
fn report_bad_format_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--format",
            "html",
        ])
        .assert()
        .failure()
        .stderr(contains("unsupported report format"));
}

#[test]
fn report_format_md_alias() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();
    {
        let conn = osl::db::open(&vault).unwrap();
        let day = today_iso_date(&conn);
        insert_session(
            &conn,
            &format!("{day}T10:00:00Z"),
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--format",
            "md",
        ])
        .assert()
        .success()
        .stdout(contains("# Usage Report"));
}

#[test]
fn report_empty_save_inserts_no_row() {
    let (_tmp, mut conn) = open_tmp();
    osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        true,
    )
    .unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM reports", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn report_last_30_days_json_period_kind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();
    {
        let conn = osl::db::open(&vault).unwrap();
        let day = today_iso_date(&conn);
        insert_session(
            &conn,
            &format!("{day}T10:00:00Z"),
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    let out = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "last-30-days",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["period_kind"], "last-30-days");
}

#[test]
fn report_weekly_rolling_window() {
    let (_tmp, mut conn) = open_tmp();
    let outside = date_offset(&conn, "-8 days");
    let inside = date_offset(&conn, "-3 days");

    insert_session(
        &conn,
        &format!("{outside}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        &format!("{inside}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Weekly),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 1);
}

#[test]
fn report_monthly_calendar_window() {
    let (_tmp, mut conn) = open_tmp();
    let start_of_month: String = conn
        .query_row("SELECT date('now','start of month')", [], |r| r.get(0))
        .unwrap();
    let prev_month: String = conn
        .query_row("SELECT date('now','start of month','-1 month')", [], |r| {
            r.get(0)
        })
        .unwrap();

    insert_session(
        &conn,
        &format!("{prev_month}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        &format!("{start_of_month}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Monthly),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 1);
}

#[test]
fn report_scope_strings() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let global = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(global.scope, "global");

    let project = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        Some("alpha"),
        None,
        false,
    )
    .unwrap();
    assert_eq!(project.scope, "project:alpha");

    let source = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        Some("claude"),
        false,
    )
    .unwrap();
    assert_eq!(source.scope, "source:claude");

    let both = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        Some("alpha"),
        Some("claude"),
        false,
    )
    .unwrap();
    assert_eq!(both.scope, "project:alpha;source:claude");
}

#[test]
fn report_combined_project_and_source_filter() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        Some("beta"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "codex",
        Some("alpha"),
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        Some("alpha"),
        Some("claude"),
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.total_sessions, 1);
    assert_eq!(doc.scope, "project:alpha;source:claude");
}

#[test]
fn report_top_tools_orders_by_count() {
    let (_tmp, mut conn) = open_tmp();
    for _ in 0..3 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &[],
            &["Bash"],
            0,
        );
    }
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &[],
        &["Read"],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.top_tools[0].tool_name, "Bash");
    assert_eq!(doc.metrics.top_tools[0].count, 3);
    assert_eq!(doc.metrics.top_tools[1].tool_name, "Read");
    assert_eq!(doc.metrics.top_tools[1].count, 1);
}

#[test]
fn report_messages_by_role_ordered_desc() {
    let (_tmp, mut conn) = open_tmp();
    for _ in 0..4 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }
    for _ in 0..2 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["assistant"],
            &[],
            0,
        );
    }

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.messages_by_role[0].role, "user");
    assert_eq!(doc.metrics.messages_by_role[0].count, 4);
    assert_eq!(doc.metrics.messages_by_role[1].role, "assistant");
    assert_eq!(doc.metrics.messages_by_role[1].count, 2);
}

#[test]
fn report_estimated_cost_renders_dollar() {
    let (_tmp, mut conn) = open_tmp();
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        Some(1.2345),
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    assert_eq!(doc.metrics.estimated_cost_usd, Some(1.2345));
    let md = osl::report::render_markdown(&doc);
    assert!(md.contains("$1.23"));
}

#[test]
fn report_sources_breakdown_counts() {
    let (_tmp, mut conn) = open_tmp();
    for _ in 0..3 {
        insert_session(
            &conn,
            "2026-06-05T10:00:00Z",
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }
    insert_session(
        &conn,
        "2026-06-05T10:00:00Z",
        None,
        "codex",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    let by_name: HashMap<String, i64> = doc
        .metrics
        .sources
        .iter()
        .map(|s| (s.source.clone(), s.session_count))
        .collect();
    assert_eq!(by_name.get("claude").copied().unwrap_or(0), 3);
    assert_eq!(by_name.get("codex").copied().unwrap_or(0), 1);
}

#[test]
fn report_daily_breakdown_empty_renders_none() {
    let (_tmp, mut conn) = open_tmp();
    let doc = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Custom),
        Some("2026-06-01"),
        Some("2026-06-30"),
        None,
        None,
        false,
    )
    .unwrap();

    let md = osl::report::render_markdown(&doc);
    assert!(md.contains("## Daily breakdown"));
    assert!(md.contains("_(none)_"));
}

#[test]
fn report_open_period_not_cached_without_save() {
    let (_tmp, mut conn) = open_tmp();
    let day = today_iso_date(&conn);
    insert_session(
        &conn,
        &format!("{day}T10:00:00Z"),
        None,
        "claude",
        None,
        "m1",
        10,
        10,
        0,
        0,
        None,
        &["user"],
        &[],
        0,
    );

    let first = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(!first.from_cache);

    let second = osl::report::run_report(
        &mut conn,
        Some(ReportPeriodKind::Daily),
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();
    assert!(!second.from_cache);
}

#[test]
fn report_json_has_top_level_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    osl::db::init(&vault, true).unwrap();
    {
        let conn = osl::db::open(&vault).unwrap();
        let day = today_iso_date(&conn);
        insert_session(
            &conn,
            &format!("{day}T10:00:00Z"),
            None,
            "claude",
            None,
            "m1",
            10,
            10,
            0,
            0,
            None,
            &["user"],
            &[],
            0,
        );
    }

    let out = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "report",
            "--period",
            "daily",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json.get("scope").is_some());
    assert!(json.get("period_start").is_some());
    assert!(json.get("period_end").is_some());
    assert!(json.get("generated_at").is_some());
    assert!(json.get("from_cache").is_some());
    assert!(json.get("metrics").is_some());
}
